//! Authenticated local named-pipe transport for the optional container broker.
//!
//! The pipe is scoped to one interactive logon SID and rejects remote clients.
//! Both ends authenticate the opposite Windows boundary: the broker checks the
//! impersonated user/logon/session token, while the client checks the server
//! PID against the installed SCM service. The service listener requires its
//! dedicated service SID. The first control message is a bounded protocol
//! Hello; no execution request is read until peer authentication succeeds.

mod peer_security;

use super::broker_protocol::{
    BROKER_PROTOCOL_VERSION, BrokerControlMessage, BrokerFrame, BrokerFrameKind,
    MAX_CONTROL_PAYLOAD_BYTES, negotiate_version,
};
use peer_security::{
    ActiveSessionIdentity, PipeSecurityDescriptor, current_process_identity,
    impersonate_and_read_peer, query_unique_active_session, validate_sid_bytes,
    verify_current_service_identity, verify_pipe_server,
};
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile,
    SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
};

const BROKER_PIPE_NAME: &str = r"\\.\pipe\Haven.ContainerBroker.v1";
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const MAX_HANDSHAKE_BODY_BYTES: usize = 1_024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// OS identity expected on the local client side of the broker pipe.
///
/// The service SID identifies the installed broker executable through its SCM
/// process token. The logon SID scopes pipe access to one interactive logon;
/// user SID and session ID are checked again after pipe impersonation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrokerPeerPolicy {
    user_sid: Vec<u8>,
    logon_sid: Vec<u8>,
    session_id: u32,
    service_sid: Vec<u8>,
}

impl BrokerPeerPolicy {
    /// Build the listener policy from the sole active interactive WTS session.
    /// The service identity is supplied by the broker's fixed SCM configuration;
    /// user and logon SIDs are never accepted from a renderer or pipe payload.
    pub(crate) fn from_active_session(service_sid: Vec<u8>) -> io::Result<Self> {
        validate_sid_bytes(&service_sid)?;
        let ActiveSessionIdentity {
            session_id,
            user_sid,
            logon_sid,
        } = query_unique_active_session()?;
        Self::from_parts(user_sid, logon_sid, session_id, service_sid)
    }

    #[cfg(test)]
    pub(crate) fn new(
        user_sid: Vec<u8>,
        logon_sid: Vec<u8>,
        session_id: u32,
        service_sid: Vec<u8>,
    ) -> io::Result<Self> {
        Self::from_parts(user_sid, logon_sid, session_id, service_sid)
    }

    fn from_parts(
        user_sid: Vec<u8>,
        logon_sid: Vec<u8>,
        session_id: u32,
        service_sid: Vec<u8>,
    ) -> io::Result<Self> {
        validate_sid_bytes(&user_sid)?;
        validate_sid_bytes(&logon_sid)?;
        validate_sid_bytes(&service_sid)?;
        if session_id == 0 {
            return Err(invalid_input("broker client session must be interactive"));
        }
        Ok(Self {
            user_sid,
            logon_sid,
            session_id,
            service_sid,
        })
    }

    fn authorize(&self, identity: &BrokerPeerIdentity) -> io::Result<()> {
        if identity.process_id == 0
            || identity.session_id != self.session_id
            || identity.user_sid != self.user_sid
            || identity.logon_sid != self.logon_sid
        {
            return Err(permission_denied(
                "broker named-pipe peer is not authorized",
            ));
        }
        Ok(())
    }
}

/// Client-side identity proof derived from the current process token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrokerClientPolicy {
    user_sid: Vec<u8>,
    logon_sid: Vec<u8>,
    session_id: u32,
}

impl BrokerClientPolicy {
    /// Bind the client to its current Windows logon token without accepting
    /// identity fields from its caller.
    pub(crate) fn for_current_process() -> io::Result<Self> {
        let identity = current_process_identity()?;
        validate_sid_bytes(&identity.user_sid)?;
        validate_sid_bytes(&identity.logon_sid)?;
        if identity.session_id == 0 {
            return Err(invalid_input(
                "broker client must run in an interactive session",
            ));
        }
        Ok(Self {
            user_sid: identity.user_sid,
            logon_sid: identity.logon_sid,
            session_id: identity.session_id,
        })
    }

    fn authorize(&self, identity: &BrokerPeerIdentity) -> io::Result<()> {
        if identity.process_id == 0
            || identity.session_id != self.session_id
            || identity.user_sid != self.user_sid
            || identity.logon_sid != self.logon_sid
        {
            return Err(permission_denied(
                "broker client process identity is invalid",
            ));
        }
        Ok(())
    }
}

/// Kernel-derived identity for the peer connected to one pipe instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrokerPeerIdentity {
    process_id: u32,
    session_id: u32,
    user_sid: Vec<u8>,
    logon_sid: Vec<u8>,
}

impl BrokerPeerIdentity {
    pub(crate) fn process_id(&self) -> u32 {
        self.process_id
    }

    pub(crate) fn session_id(&self) -> u32 {
        self.session_id
    }
}

/// One serialized broker listener. Keeping the server pipe handle open while
/// disconnected prevents another local process from squatting the pipe name.
pub(crate) struct BrokerPipeListener {
    handle: HANDLE,
    policy: BrokerPeerPolicy,
}

impl BrokerPipeListener {
    pub(crate) fn new(policy: BrokerPeerPolicy) -> io::Result<Self> {
        verify_current_service_identity(&policy.service_sid)?;
        Self::create(BROKER_PIPE_NAME, policy)
    }

    fn create(name: &str, policy: BrokerPeerPolicy) -> io::Result<Self> {
        let name = wide_null(name);
        let security = PipeSecurityDescriptor::for_policy(&policy)?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: security.as_ptr(),
            bInheritHandle: 0,
        };
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                PIPE_BUFFER_BYTES,
                PIPE_BUFFER_BYTES,
                HANDSHAKE_TIMEOUT.as_millis() as u32,
                &attributes,
            )
        };
        if handle == INVALID_HANDLE_VALUE || handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { handle, policy })
    }

    /// Wait for a client, require its Hello, authenticate its impersonation
    /// token and only then acknowledge the protocol session.
    pub(crate) fn accept(&mut self) -> io::Result<BrokerPipeConnection<'_>> {
        let connected = unsafe { ConnectNamedPipe(self.handle, null_mut()) };
        if connected == 0 {
            let error = unsafe { GetLastError() };
            if error != ERROR_PIPE_CONNECTED {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
        }

        let result = self.authenticate_and_acknowledge();
        match result {
            Ok(peer) => Ok(BrokerPipeConnection {
                listener: self,
                peer,
            }),
            Err(error) => {
                unsafe { DisconnectNamedPipe(self.handle) };
                Err(error)
            }
        }
    }

    fn authenticate_and_acknowledge(&mut self) -> io::Result<BrokerPeerIdentity> {
        let hello = read_bounded_hello(self.handle)?;
        let (minimum_version, maximum_version) = match hello {
            BrokerControlMessage::Hello {
                minimum_version,
                maximum_version,
            } => (minimum_version, maximum_version),
            _ => return Err(invalid_data("first broker message must be Hello")),
        };
        let selected_version = negotiate_version(minimum_version, maximum_version)?;

        let peer = impersonate_and_read_peer(self.handle)?;
        self.policy.authorize(&peer)?;
        BrokerFrame::control(&BrokerControlMessage::HelloAccepted { selected_version })?
            .write_to(&mut PipeBorrow(self.handle))?;
        Ok(peer)
    }
}

impl Drop for BrokerPipeListener {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

/// Authenticated server stream. Dropping it disconnects the client before the
/// listener can accept another request.
pub(crate) struct BrokerPipeConnection<'a> {
    listener: &'a mut BrokerPipeListener,
    peer: BrokerPeerIdentity,
}

impl BrokerPipeConnection<'_> {
    pub(crate) fn peer(&self) -> &BrokerPeerIdentity {
        &self.peer
    }

    pub(crate) fn read_frame(&mut self, timeout: Duration) -> io::Result<BrokerFrame> {
        read_pipe_frame(self.listener.handle, timeout)
    }

    pub(crate) fn write_frame(&mut self, frame: &BrokerFrame) -> io::Result<()> {
        frame.write_to(&mut PipeBorrow(self.listener.handle))
    }
}

impl Drop for BrokerPipeConnection<'_> {
    fn drop(&mut self) {
        unsafe { DisconnectNamedPipe(self.listener.handle) };
    }
}

/// Client stream authenticated against the installed SCM service process.
pub(crate) struct BrokerPipeClient {
    handle: HANDLE,
}

impl BrokerPipeClient {
    pub(crate) fn connect(policy: &BrokerClientPolicy) -> io::Result<Self> {
        let current = current_process_identity()?;
        policy.authorize(&current)?;
        let handle = connect_client_pipe(BROKER_PIPE_NAME)?;
        let mut client = Self { handle };
        verify_pipe_server(handle)?;

        client.write_frame(&BrokerFrame::control(&BrokerControlMessage::Hello {
            minimum_version: BROKER_PROTOCOL_VERSION,
            maximum_version: BROKER_PROTOCOL_VERSION,
        })?)?;
        match client.read_frame(HANDSHAKE_TIMEOUT)?.decode_control()? {
            BrokerControlMessage::HelloAccepted { selected_version }
                if selected_version == BROKER_PROTOCOL_VERSION =>
            {
                Ok(client)
            }
            _ => Err(permission_denied(
                "broker service did not accept the protocol session",
            )),
        }
    }

    #[cfg(test)]
    fn connect_for_test(name: &str) -> io::Result<Self> {
        let handle = connect_client_pipe(name)?;
        let mut client = Self { handle };
        client.write_frame(&BrokerFrame::control(&BrokerControlMessage::Hello {
            minimum_version: BROKER_PROTOCOL_VERSION,
            maximum_version: BROKER_PROTOCOL_VERSION,
        })?)?;
        match client.read_frame(HANDSHAKE_TIMEOUT)?.decode_control()? {
            BrokerControlMessage::HelloAccepted { selected_version }
                if selected_version == BROKER_PROTOCOL_VERSION =>
            {
                Ok(client)
            }
            _ => Err(permission_denied(
                "test broker did not accept the protocol session",
            )),
        }
    }
}

impl BrokerPipeClient {
    pub(crate) fn read_frame(&mut self, timeout: Duration) -> io::Result<BrokerFrame> {
        read_pipe_frame(self.handle, timeout)
    }

    pub(crate) fn write_frame(&mut self, frame: &BrokerFrame) -> io::Result<()> {
        frame.write_to(&mut PipeBorrow(self.handle))
    }
}

impl Drop for BrokerPipeClient {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

struct PipeBorrow(HANDLE);

impl Read for PipeBorrow {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_pipe(self.0, buffer)
    }
}

impl Write for PipeBorrow {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        write_pipe(self.0, buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_pipe(handle: HANDLE, buffer: &mut [u8]) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let requested = u32::try_from(buffer.len())
        .map_err(|_| invalid_data("named-pipe read buffer is outside its limit"))?;
    let mut bytes_read = 0;
    let succeeded = unsafe {
        ReadFile(
            handle,
            buffer.as_mut_ptr(),
            requested,
            &mut bytes_read,
            null_mut::<OVERLAPPED>(),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(bytes_read as usize)
}

fn write_pipe(handle: HANDLE, buffer: &[u8]) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let requested = u32::try_from(buffer.len())
        .map_err(|_| invalid_data("named-pipe write buffer is outside its limit"))?;
    let mut bytes_written = 0;
    let succeeded = unsafe {
        WriteFile(
            handle,
            buffer.as_ptr(),
            requested,
            &mut bytes_written,
            null_mut::<OVERLAPPED>(),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(bytes_written as usize)
}

fn read_bounded_hello(handle: HANDLE) -> io::Result<BrokerControlMessage> {
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut length_bytes = [0_u8; 4];
    read_exact_pipe_until(handle, &mut length_bytes, deadline)?;
    let body_length = u32::from_le_bytes(length_bytes) as usize;
    if !(1..=MAX_HANDSHAKE_BODY_BYTES).contains(&body_length) {
        return Err(invalid_data("broker Hello frame exceeds its limit"));
    }
    let mut body = vec![0_u8; body_length];
    read_exact_pipe_until(handle, &mut body, deadline)?;
    let kind = BrokerFrameKind::try_from(body[0])?;
    let frame = BrokerFrame::new(kind, body.split_off(1))?;
    frame.decode_control()
}

fn read_pipe_frame(handle: HANDLE, timeout: Duration) -> io::Result<BrokerFrame> {
    let deadline = Instant::now() + timeout;
    let mut length_bytes = [0_u8; 4];
    read_exact_pipe_until(handle, &mut length_bytes, deadline)?;
    let body_length = u32::from_le_bytes(length_bytes) as usize;
    if !(1..=MAX_CONTROL_PAYLOAD_BYTES + 1).contains(&body_length) {
        return Err(invalid_data("broker frame length is outside its limit"));
    }
    let mut body = vec![0_u8; body_length];
    read_exact_pipe_until(handle, &mut body, deadline)?;
    let kind = BrokerFrameKind::try_from(body[0])?;
    BrokerFrame::new(kind, body.split_off(1))
}

fn read_exact_pipe_until(
    handle: HANDLE,
    mut buffer: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    loop {
        if buffer.is_empty() {
            return Ok(());
        }
        let mut available = 0;
        let succeeded = unsafe {
            PeekNamedPipe(
                handle,
                null_mut(),
                0,
                null_mut(),
                &mut available,
                null_mut(),
            )
        };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        if available != 0 {
            let requested = (available as usize)
                .min(buffer.len())
                .min(PIPE_BUFFER_BYTES as usize);
            let read = read_pipe(handle, &mut buffer[..requested])?;
            if read == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "broker pipe closed during a frame",
                ));
            }
            buffer = &mut buffer[read..];
            continue;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "broker frame read timed out",
            ));
        }
        std::thread::sleep(HANDSHAKE_POLL_INTERVAL);
    }
}

fn connect_client_pipe(name: &str) -> io::Result<HANDLE> {
    let name = wide_null(name);
    if unsafe { WaitNamedPipeW(name.as_ptr(), 5_000) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(handle)
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::peer_security::{LocalString, MAX_SID_BYTES};
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use windows_sys::Win32::Security::GetLengthSid;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    static NEXT_TEST_PIPE: AtomicU32 = AtomicU32::new(1);

    #[test]
    fn peer_policy_binds_user_logon_session_and_nonzero_process() {
        let identity = current_process_identity().unwrap();
        let policy = BrokerPeerPolicy::new(
            identity.user_sid.clone(),
            identity.logon_sid.clone(),
            identity.session_id,
            identity.user_sid.clone(),
        )
        .unwrap();
        assert!(policy.authorize(&identity).is_ok());

        let mut wrong_user = identity.clone();
        wrong_user.user_sid = sid_from_string("S-1-5-18").unwrap();
        assert_eq!(
            policy.authorize(&wrong_user).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );

        let mut wrong_logon = identity.clone();
        wrong_logon.logon_sid = sid_from_string("S-1-5-5-1-2").unwrap();
        assert!(policy.authorize(&wrong_logon).is_err());

        let mut wrong_session = identity.clone();
        wrong_session.session_id = identity.session_id.saturating_add(1);
        assert!(policy.authorize(&wrong_session).is_err());

        let mut missing_pid = identity;
        missing_pid.process_id = 0;
        assert!(policy.authorize(&missing_pid).is_err());
    }

    #[test]
    fn client_policy_derives_identity_from_its_process_token() {
        let identity = current_process_identity().unwrap();
        let policy = BrokerClientPolicy::for_current_process().unwrap();
        assert!(policy.authorize(&identity).is_ok());
    }

    #[test]
    fn named_pipe_requires_authenticated_hello_before_returning_connection() {
        run_pipe_handshake_test(false);
    }

    #[test]
    fn named_pipe_rejects_peer_with_wrong_user_after_bounded_hello() {
        run_pipe_handshake_test(true);
    }

    #[test]
    fn named_pipe_frame_reader_times_out_when_peer_stalls() {
        let current = current_process_identity().unwrap();
        let policy = BrokerPeerPolicy::new(
            current.user_sid.clone(),
            current.logon_sid,
            current.session_id,
            current.user_sid,
        )
        .unwrap();
        let name = format!(
            r"\\.\pipe\Haven.ContainerBroker.Test.{}.{}",
            unsafe { GetCurrentProcessId() },
            NEXT_TEST_PIPE.fetch_add(1, Ordering::Relaxed)
        );
        let server_name = name.clone();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let listener = BrokerPipeListener::create(&server_name, policy).unwrap();
            ready_sender.send(()).unwrap();
            let connected = unsafe { ConnectNamedPipe(listener.handle, null_mut()) };
            if connected == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                result_sender
                    .send(Err(io::Error::last_os_error().kind()))
                    .unwrap();
                return;
            }
            let result = read_pipe_frame(listener.handle, Duration::from_millis(30))
                .map(|_| ())
                .map_err(|error| error.kind());
            result_sender.send(result).unwrap();
        });
        ready_receiver.recv().unwrap();
        let client = connect_client_pipe(&name).unwrap();
        assert_eq!(
            result_receiver.recv().unwrap().unwrap_err(),
            io::ErrorKind::TimedOut
        );
        unsafe { CloseHandle(client) };
        server.join().unwrap();
    }

    fn run_pipe_handshake_test(wrong_user: bool) {
        let current = current_process_identity().unwrap();
        let policy = BrokerPeerPolicy::new(
            if wrong_user {
                sid_from_string("S-1-5-18").unwrap()
            } else {
                current.user_sid.clone()
            },
            current.logon_sid,
            current.session_id,
            sid_from_string("S-1-5-18").unwrap(),
        )
        .unwrap();
        let name = format!(
            r"\\.\pipe\Haven.ContainerBroker.Test.{}.{}",
            unsafe { GetCurrentProcessId() },
            NEXT_TEST_PIPE.fetch_add(1, Ordering::Relaxed)
        );
        let server_name = name.clone();
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let server_policy = policy.clone();
        let server = std::thread::spawn(move || {
            let mut listener = BrokerPipeListener::create(&server_name, server_policy).unwrap();
            ready_sender.send(()).unwrap();
            match listener.accept() {
                Ok(connection) => {
                    result_sender
                        .send(Ok(connection.peer().process_id()))
                        .unwrap();
                    let _ = release_receiver.recv();
                    drop(connection);
                }
                Err(error) => result_sender.send(Err(error.kind())).unwrap(),
            }
        });
        ready_receiver.recv().unwrap();

        let client = BrokerPipeClient::connect_for_test(&name);
        if wrong_user {
            assert!(client.is_err());
            assert_eq!(
                result_receiver.recv().unwrap().unwrap_err(),
                io::ErrorKind::PermissionDenied
            );
        } else {
            let client = match client {
                Ok(client) => client,
                Err(error) => panic!(
                    "broker test client handshake failed: {error}; server result: {:?}",
                    result_receiver.recv()
                ),
            };
            let server_pid = result_receiver.recv().unwrap().unwrap();
            assert_eq!(server_pid, unsafe { GetCurrentProcessId() });
            drop(client);
            release_sender.send(()).unwrap();
        }
        server.join().unwrap();
    }

    fn sid_from_string(value: &str) -> io::Result<Vec<u8>> {
        let mut output = null_mut();
        let value = wide_null(value);
        if unsafe {
            windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW(
                value.as_ptr(),
                &mut output,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let _allocated = LocalString(output.cast());
        let length = unsafe { GetLengthSid(output) } as usize;
        if !(8..=MAX_SID_BYTES).contains(&length) {
            return Err(invalid_data("test SID length is invalid"));
        }
        Ok(unsafe { std::slice::from_raw_parts(output.cast::<u8>(), length) }.to_vec())
    }
}
