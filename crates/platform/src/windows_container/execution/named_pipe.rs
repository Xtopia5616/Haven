//! Authenticated local named-pipe transport for the optional container broker.
//!
//! The pipe is scoped to one interactive logon SID and rejects remote clients.
//! Both ends authenticate the opposite Windows boundary: the broker checks the
//! impersonated user/logon/session token, while the client checks the server
//! PID against the installed SCM service. The service listener requires its
//! dedicated service SID. The first control message is a bounded protocol
//! Hello; no execution request is read until peer authentication succeeds.

mod client_exchange;
mod peer_security;

pub(crate) use client_exchange::BrokerPipeExecutionResponse;

pub(crate) fn broker_service_sid_string() -> io::Result<String> {
    peer_security::broker_service_sid_string()
}

/// Require the fixed service identity before entering a privileged helper
/// mode. This intentionally checks both session 0 and the restricted service
/// SID, so an interactive user cannot invoke the helper under their own token.
pub(crate) fn verify_current_broker_service_identity() -> io::Result<()> {
    let service_sid = peer_security::current_broker_service_sid()?;
    peer_security::verify_current_service_identity(&service_sid)
}

#[cfg(test)]
use super::broker_protocol::BrokerFailureCode;
use super::broker_protocol::{
    BROKER_PROTOCOL_VERSION, BrokerControlMessage, BrokerExecutionGateStatus, BrokerFrame,
    BrokerFrameKind, MAX_CONTROL_PAYLOAD_BYTES, negotiate_version,
};
#[cfg(test)]
use super::broker_protocol::{BrokerQuotaGateStatus, BrokerSkillPythonRuntimeStatus};
use peer_security::{
    ActiveSessionIdentity, PipeSecurityDescriptor, current_broker_service_sid,
    current_process_identity, impersonate_and_read_peer, query_unique_active_session,
    validate_sid_bytes, verify_current_service_identity, verify_pipe_server,
};
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, GetLastError,
    HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, ReadFile, SECURITY_IMPERSONATION, SECURITY_SQOS_PRESENT, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe, WaitNamedPipeW,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

const BROKER_PIPE_NAME: &str = r"\\.\pipe\Haven.ContainerBroker.v4";
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const MAX_HANDSHAKE_BODY_BYTES: usize = 1_024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PIPE_IO_TIMEOUT: Duration = Duration::from_secs(60);

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
    /// The service SID is resolved from the fixed `NT SERVICE` account and
    /// checked against this process token. User identity comes only from WTS.
    pub(crate) fn from_active_session() -> io::Result<Self> {
        let service_sid = current_broker_service_sid()?;
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

/// Event-backed cancellation shared with the SCM control handler.
#[derive(Clone)]
pub(crate) struct BrokerPipeCancellation {
    handle: Arc<SharedEventHandle>,
}

struct SharedEventHandle(HANDLE);

// The handle names a kernel event that may be set and waited from any thread.
// Arc keeps it open until all concurrent cancellation users have finished.
unsafe impl Send for SharedEventHandle {}
unsafe impl Sync for SharedEventHandle {}

impl Drop for SharedEventHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

impl BrokerPipeCancellation {
    pub(crate) fn new() -> io::Result<Self> {
        let handle = unsafe { CreateEventW(null::<SECURITY_ATTRIBUTES>(), 1, 0, null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle: Arc::new(SharedEventHandle(handle)),
        })
    }

    pub(crate) fn cancel(&self) -> io::Result<()> {
        if unsafe { SetEvent(self.handle.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        (unsafe { WaitForSingleObject(self.handle.0, 0) }) != WAIT_TIMEOUT
    }

    pub(crate) fn raw_handle(&self) -> HANDLE {
        self.handle.0
    }
}

impl BrokerPeerIdentity {
    #[cfg(test)]
    pub(crate) fn process_id(&self) -> u32 {
        self.process_id
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
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
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
    pub(crate) fn accept<'a>(
        &'a mut self,
        cancellation: &'a BrokerPipeCancellation,
    ) -> io::Result<BrokerPipeConnection<'a>> {
        if let Err(error) = connect_named_pipe_until(
            self.handle,
            cancellation,
            Instant::now() + HANDSHAKE_TIMEOUT,
        ) {
            unsafe { DisconnectNamedPipe(self.handle) };
            return Err(error);
        }

        let result = self.authenticate_and_acknowledge(cancellation);
        match result {
            Ok(peer) => {
                #[cfg(not(test))]
                let _ = peer;
                Ok(BrokerPipeConnection {
                    listener: self,
                    #[cfg(test)]
                    peer,
                    cancellation,
                })
            }
            Err(error) => {
                unsafe { DisconnectNamedPipe(self.handle) };
                Err(error)
            }
        }
    }

    fn authenticate_and_acknowledge(
        &mut self,
        cancellation: &BrokerPipeCancellation,
    ) -> io::Result<BrokerPeerIdentity> {
        let hello = read_bounded_hello(self.handle, cancellation)?;
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
        let acknowledgement =
            BrokerFrame::control(&BrokerControlMessage::HelloAccepted { selected_version })?;
        acknowledgement.write_to(&mut PipeBorrow {
            handle: self.handle,
            cancellation,
            deadline: Instant::now() + HANDSHAKE_TIMEOUT,
        })?;
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
    #[cfg(test)]
    peer: BrokerPeerIdentity,
    cancellation: &'a BrokerPipeCancellation,
}

impl BrokerPipeConnection<'_> {
    #[cfg(test)]
    pub(crate) fn peer(&self) -> &BrokerPeerIdentity {
        &self.peer
    }

    pub(crate) fn cancellation(&self) -> BrokerPipeCancellation {
        self.cancellation.clone()
    }

    pub(crate) fn read_frame(&self, timeout: Duration) -> io::Result<BrokerFrame> {
        read_pipe_frame(self.listener.handle, timeout, self.cancellation)
    }

    pub(crate) fn read_frame_until(&self, deadline: Instant) -> io::Result<BrokerFrame> {
        read_pipe_frame_until(self.listener.handle, deadline, self.cancellation)
    }

    pub(crate) fn write_frame(&self, frame: &BrokerFrame, timeout: Duration) -> io::Result<()> {
        frame.write_to(&mut PipeBorrow {
            handle: self.listener.handle,
            cancellation: self.cancellation,
            deadline: deadline_after(timeout)?,
        })
    }

    pub(crate) fn write_frame_until(
        &self,
        frame: &BrokerFrame,
        deadline: Instant,
    ) -> io::Result<()> {
        frame.write_to(&mut PipeBorrow {
            handle: self.listener.handle,
            cancellation: self.cancellation,
            deadline,
        })
    }

    /// Treat any failed kernel probe as a disconnected peer so a running
    /// guest is stopped when its requesting client disappears.
    pub(crate) fn peer_disconnected(&self) -> bool {
        let mut available_bytes = 0;
        unsafe {
            PeekNamedPipe(
                self.listener.handle,
                null_mut(),
                0,
                null_mut(),
                &mut available_bytes,
                null_mut(),
            ) == 0
        }
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
    cancellation: BrokerPipeCancellation,
}

impl BrokerPipeClient {
    /// Query whether the authenticated service's startup quota gate is open.
    pub(crate) fn execution_gate_status(&mut self) -> io::Result<BrokerExecutionGateStatus> {
        self.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::ExecutionGateStatusRequest)?,
            HANDSHAKE_TIMEOUT,
        )?;
        match self.read_frame(HANDSHAKE_TIMEOUT)?.decode_control()? {
            BrokerControlMessage::ExecutionGateStatus { status } => {
                self.write_frame(
                    &BrokerFrame::control(&BrokerControlMessage::ExecutionGateStatusAcknowledged)?,
                    HANDSHAKE_TIMEOUT,
                )?;
                Ok(status)
            }
            _ => Err(invalid_data(
                "broker did not return its execution gate status",
            )),
        }
    }

    pub(crate) fn connect(
        policy: &BrokerClientPolicy,
        cancellation: BrokerPipeCancellation,
    ) -> io::Result<Self> {
        let current = current_process_identity()?;
        policy.authorize(&current)?;
        let handle = connect_client_pipe(BROKER_PIPE_NAME)?;
        let mut client = Self {
            handle,
            cancellation,
        };
        verify_pipe_server(handle)?;

        client.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::Hello {
                minimum_version: BROKER_PROTOCOL_VERSION,
                maximum_version: BROKER_PROTOCOL_VERSION,
            })?,
            HANDSHAKE_TIMEOUT,
        )?;
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
        let cancellation = BrokerPipeCancellation::new()?;
        Self::connect_for_test_with_cancellation(name, cancellation)
    }

    #[cfg(test)]
    fn connect_for_test_with_cancellation(
        name: &str,
        cancellation: BrokerPipeCancellation,
    ) -> io::Result<Self> {
        let handle = connect_client_pipe(name)?;
        let mut client = Self {
            handle,
            cancellation,
        };
        client.write_frame(
            &BrokerFrame::control(&BrokerControlMessage::Hello {
                minimum_version: BROKER_PROTOCOL_VERSION,
                maximum_version: BROKER_PROTOCOL_VERSION,
            })?,
            HANDSHAKE_TIMEOUT,
        )?;
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
    /// Send one execution request and its bounded workspace snapshot over a
    /// fresh authenticated connection. The connection is consumed so a
    /// partially sent workspace can never be reused for another request.
    pub(crate) fn execute_with_workspace(
        &mut self,
        request: &super::WindowsContainerExecutionRequest,
        request_id: [u8; 16],
        workspace: &super::WindowsContainerWorkspaceInput,
        before_shell_start: &mut dyn FnMut() -> io::Result<()>,
        shell_started: &mut dyn FnMut(),
        output: super::WindowsContainerOutputSink<'_>,
    ) -> io::Result<client_exchange::BrokerPipeExecutionResponse> {
        client_exchange::execute_with_workspace(
            self,
            request,
            request_id,
            workspace,
            before_shell_start,
            shell_started,
            output,
        )
    }

    pub(crate) fn read_frame(&mut self, timeout: Duration) -> io::Result<BrokerFrame> {
        read_pipe_frame(self.handle, timeout, &self.cancellation)
    }

    pub(crate) fn read_frame_until(&mut self, deadline: Instant) -> io::Result<BrokerFrame> {
        read_pipe_frame_until(self.handle, deadline, &self.cancellation)
    }

    pub(crate) fn write_frame(&mut self, frame: &BrokerFrame, timeout: Duration) -> io::Result<()> {
        frame.write_to(&mut PipeBorrow {
            handle: self.handle,
            cancellation: &self.cancellation,
            deadline: deadline_after(timeout)?,
        })
    }

    pub(crate) fn write_frame_until(
        &mut self,
        frame: &BrokerFrame,
        deadline: Instant,
    ) -> io::Result<()> {
        frame.write_to(&mut PipeBorrow {
            handle: self.handle,
            cancellation: &self.cancellation,
            deadline,
        })
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

struct PipeBorrow<'a> {
    handle: HANDLE,
    cancellation: &'a BrokerPipeCancellation,
    deadline: Instant,
}

impl Read for PipeBorrow<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        read_pipe_until(self.handle, buffer, self.cancellation, self.deadline)
    }
}

impl Write for PipeBorrow<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        write_pipe_until(self.handle, buffer, self.cancellation, self.deadline)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_pipe_until(
    handle: HANDLE,
    buffer: &mut [u8],
    cancellation: &BrokerPipeCancellation,
    deadline: Instant,
) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let requested = u32::try_from(buffer.len())
        .map_err(|_| invalid_data("named-pipe read buffer is outside its limit"))?;
    overlapped_pipe_io(
        handle,
        buffer.as_mut_ptr().cast(),
        requested,
        cancellation,
        deadline,
        false,
    )
}

fn write_pipe_until(
    handle: HANDLE,
    buffer: &[u8],
    cancellation: &BrokerPipeCancellation,
    deadline: Instant,
) -> io::Result<usize> {
    if buffer.is_empty() {
        return Ok(0);
    }
    let requested = u32::try_from(buffer.len())
        .map_err(|_| invalid_data("named-pipe write buffer is outside its limit"))?;
    overlapped_pipe_io(
        handle,
        buffer.as_ptr().cast_mut().cast(),
        requested,
        cancellation,
        deadline,
        true,
    )
}

fn read_bounded_hello(
    handle: HANDLE,
    cancellation: &BrokerPipeCancellation,
) -> io::Result<BrokerControlMessage> {
    let deadline = deadline_after(HANDSHAKE_TIMEOUT)?;
    let mut length_bytes = [0_u8; 4];
    read_exact_pipe_until(handle, &mut length_bytes, deadline, cancellation)?;
    let body_length = u32::from_le_bytes(length_bytes) as usize;
    if !(1..=MAX_HANDSHAKE_BODY_BYTES).contains(&body_length) {
        return Err(invalid_data("broker Hello frame exceeds its limit"));
    }
    let mut body = vec![0_u8; body_length];
    read_exact_pipe_until(handle, &mut body, deadline, cancellation)?;
    let kind = BrokerFrameKind::try_from(body[0])?;
    let frame = BrokerFrame::new(kind, body.split_off(1))?;
    frame.decode_control()
}

fn read_pipe_frame(
    handle: HANDLE,
    timeout: Duration,
    cancellation: &BrokerPipeCancellation,
) -> io::Result<BrokerFrame> {
    read_pipe_frame_until(handle, deadline_after(timeout)?, cancellation)
}

fn read_pipe_frame_until(
    handle: HANDLE,
    deadline: Instant,
    cancellation: &BrokerPipeCancellation,
) -> io::Result<BrokerFrame> {
    let mut length_bytes = [0_u8; 4];
    read_exact_pipe_until(handle, &mut length_bytes, deadline, cancellation)?;
    let body_length = u32::from_le_bytes(length_bytes) as usize;
    if !(1..=MAX_CONTROL_PAYLOAD_BYTES + 1).contains(&body_length) {
        return Err(invalid_data("broker frame length is outside its limit"));
    }
    let mut body = vec![0_u8; body_length];
    read_exact_pipe_until(handle, &mut body, deadline, cancellation)?;
    let kind = BrokerFrameKind::try_from(body[0])?;
    BrokerFrame::new(kind, body.split_off(1))
}

fn read_exact_pipe_until(
    handle: HANDLE,
    mut buffer: &mut [u8],
    deadline: Instant,
    cancellation: &BrokerPipeCancellation,
) -> io::Result<()> {
    loop {
        if buffer.is_empty() {
            return Ok(());
        }
        let read = read_pipe_until(handle, buffer, cancellation, deadline)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker pipe closed during a frame",
            ));
        }
        buffer = &mut buffer[read..];
    }
}

fn deadline_after(timeout: Duration) -> io::Result<Instant> {
    if timeout > MAX_PIPE_IO_TIMEOUT {
        return Err(invalid_input("broker pipe timeout exceeds its limit"));
    }
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| invalid_input("broker pipe timeout is invalid"))
}

fn overlapped_pipe_io(
    handle: HANDLE,
    buffer: *mut core::ffi::c_void,
    requested: u32,
    cancellation: &BrokerPipeCancellation,
    deadline: Instant,
    write: bool,
) -> io::Result<usize> {
    ensure_not_cancelled(cancellation)?;
    if Instant::now() >= deadline {
        return Err(timed_out("broker pipe I/O timed out"));
    }
    let event = unsafe { CreateEventW(null::<SECURITY_ATTRIBUTES>(), 1, 0, null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    let event = OwnedHandle(event);
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..OVERLAPPED::default()
    };
    let mut immediate_transferred = 0;
    let started = unsafe {
        if write {
            WriteFile(
                handle,
                buffer.cast_const().cast(),
                requested,
                &mut immediate_transferred,
                &mut overlapped,
            )
        } else {
            ReadFile(
                handle,
                buffer.cast(),
                requested,
                &mut immediate_transferred,
                &mut overlapped,
            )
        }
    };
    if started != 0 {
        return Ok(immediate_transferred as usize);
    }
    let error = unsafe { GetLastError() };
    if error != ERROR_IO_PENDING {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    wait_for_overlapped(handle, &overlapped, event.0, cancellation, deadline)?;
    let mut transferred = 0;
    if unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(transferred as usize)
}

fn connect_named_pipe_until(
    handle: HANDLE,
    cancellation: &BrokerPipeCancellation,
    deadline: Instant,
) -> io::Result<()> {
    ensure_not_cancelled(cancellation)?;
    let event = unsafe { CreateEventW(null::<SECURITY_ATTRIBUTES>(), 1, 0, null()) };
    if event.is_null() {
        return Err(io::Error::last_os_error());
    }
    let event = OwnedHandle(event);
    let mut overlapped = OVERLAPPED {
        hEvent: event.0,
        ..OVERLAPPED::default()
    };
    if unsafe { ConnectNamedPipe(handle, &mut overlapped) } != 0 {
        return Ok(());
    }
    let error = unsafe { GetLastError() };
    if error == ERROR_PIPE_CONNECTED {
        return Ok(());
    }
    if error != ERROR_IO_PENDING {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    wait_for_overlapped(handle, &overlapped, event.0, cancellation, deadline)?;
    let mut transferred = 0;
    if unsafe { GetOverlappedResult(handle, &overlapped, &mut transferred, 0) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wait_for_overlapped(
    handle: HANDLE,
    overlapped: &OVERLAPPED,
    event: HANDLE,
    cancellation: &BrokerPipeCancellation,
    deadline: Instant,
) -> io::Result<()> {
    let handles = [cancellation.raw_handle(), event];
    let remaining = deadline.saturating_duration_since(Instant::now());
    let timeout_ms = remaining.as_millis().min((u32::MAX - 1) as u128) as u32;
    match unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, timeout_ms) } {
        WAIT_OBJECT_0 => {
            cancel_overlapped(handle, overlapped, event)?;
            Err(interrupted("broker pipe I/O was cancelled"))
        }
        value if value == WAIT_OBJECT_0 + 1 => Ok(()),
        WAIT_TIMEOUT => {
            cancel_overlapped(handle, overlapped, event)?;
            Err(timed_out("broker pipe I/O timed out"))
        }
        WAIT_FAILED => {
            let error = io::Error::last_os_error();
            cancel_overlapped(handle, overlapped, event)?;
            Err(error)
        }
        _ => {
            cancel_overlapped(handle, overlapped, event)?;
            Err(invalid_data("broker pipe wait returned an invalid result"))
        }
    }
}

fn cancel_overlapped(handle: HANDLE, overlapped: &OVERLAPPED, event: HANDLE) -> io::Result<()> {
    let cancel_error = if unsafe { CancelIoEx(handle, overlapped) } == 0 {
        let error = unsafe { GetLastError() };
        (error != windows_sys::Win32::Foundation::ERROR_NOT_FOUND)
            .then(|| io::Error::from_raw_os_error(error as i32))
    } else {
        None
    };
    if unsafe { WaitForSingleObject(event, u32::MAX) } != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }
    if let Some(error) = cancel_error {
        return Err(error);
    }
    Ok(())
}

fn ensure_not_cancelled(cancellation: &BrokerPipeCancellation) -> io::Result<()> {
    match unsafe { WaitForSingleObject(cancellation.raw_handle(), 0) } {
        WAIT_OBJECT_0 => Err(interrupted("broker pipe I/O was cancelled")),
        WAIT_TIMEOUT => Ok(()),
        WAIT_FAILED => Err(io::Error::last_os_error()),
        _ => Err(invalid_data(
            "broker cancellation wait returned an invalid result",
        )),
    }
}

fn interrupted(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, message)
}

fn timed_out(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, message)
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
            SECURITY_SQOS_PRESENT | SECURITY_IMPERSONATION | FILE_FLAG_OVERLAPPED,
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
    use std::path::Path;
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
    fn broker_execution_gate_status_uses_the_authenticated_protocol_connection() {
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
        let server = std::thread::spawn(move || -> io::Result<()> {
            let mut listener = BrokerPipeListener::create(&server_name, policy)?;
            let cancellation = BrokerPipeCancellation::new()?;
            ready_sender.send(()).unwrap();
            let connection = listener.accept(&cancellation)?;
            if !matches!(
                connection
                    .read_frame(Duration::from_secs(5))?
                    .decode_control()?,
                BrokerControlMessage::ExecutionGateStatusRequest
            ) {
                return Err(invalid_data("test client sent an invalid status request"));
            }
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::ExecutionGateStatus {
                    status: BrokerExecutionGateStatus {
                        quota: BrokerQuotaGateStatus::Unattested,
                        skill_python_runtime: BrokerSkillPythonRuntimeStatus::Unattested,
                    },
                })?,
                Duration::from_secs(5),
            )?;
            match connection
                .read_frame(Duration::from_secs(5))?
                .decode_control()?
            {
                BrokerControlMessage::ExecutionGateStatusAcknowledged => Ok(()),
                _ => Err(invalid_data("test client did not acknowledge status")),
            }
        });
        ready_receiver.recv().unwrap();

        let mut client = BrokerPipeClient::connect_for_test(&name).unwrap();
        assert_eq!(
            client.execution_gate_status().unwrap(),
            BrokerExecutionGateStatus {
                quota: BrokerQuotaGateStatus::Unattested,
                skill_python_runtime: BrokerSkillPythonRuntimeStatus::Unattested,
            }
        );
        server.join().unwrap().unwrap();
    }

    #[test]
    fn broker_client_and_receiver_exchange_a_bounded_workspace_snapshot() {
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
        let server = std::thread::spawn(move || -> io::Result<(u64, Vec<u8>)> {
            let mut listener = BrokerPipeListener::create(&server_name, policy)?;
            let cancellation = BrokerPipeCancellation::new()?;
            ready_sender.send(()).unwrap();
            let connection = listener.accept(&cancellation)?;
            let request = match connection
                .read_frame(Duration::from_secs(5))?
                .decode_control()?
            {
                BrokerControlMessage::Execute { request } => request,
                _ => return Err(invalid_data("test client sent an invalid request")),
            };
            let request_id = request.request_id;
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::WorkspaceSnapshotRequested {
                    request_id,
                })?,
                Duration::from_secs(5),
            )?;

            let staging_temp = tempfile::tempdir()?;
            let staging = crate::filesystem::DirectoryCapability::open(staging_temp.path())?;
            let copy_limits = request.validate()?.workspace_copy();
            let cancellation = connection.cancellation();
            let (snapshot, summary) = super::super::workspace_transfer::receive_workspace_snapshot(
                staging,
                copy_limits,
                || connection.read_frame(Duration::from_secs(5)),
                || cancellation.is_cancelled(),
            )?;
            let bytes = std::fs::read(snapshot.root().join("payload.txt"))?;
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::Failure {
                    request_id: Some(request_id),
                    code: BrokerFailureCode::BackendUnavailable,
                })?,
                Duration::from_secs(5),
            )?;
            if !matches!(
                connection
                    .read_frame(Duration::from_secs(5))?
                    .decode_control()?,
                BrokerControlMessage::ExecutionAcknowledged {
                    request_id: acknowledged_id
                } if acknowledged_id == request_id
            ) {
                return Err(invalid_data("test client did not acknowledge failure"));
            }
            Ok((summary.byte_count, bytes))
        });
        ready_receiver.recv().unwrap();

        let workspace_temp = tempfile::tempdir().unwrap();
        std::fs::write(workspace_temp.path().join("payload.txt"), b"payload").unwrap();
        let workspace = super::super::WindowsContainerWorkspaceInput::new(
            crate::filesystem::DirectoryCapability::open(workspace_temp.path()).unwrap(),
        );
        let copy_limits =
            crate::filesystem::DirectoryTreeCopyLimits::new(16, 8, 4096, 8192).unwrap();
        let diff_limits =
            crate::filesystem::DirectoryTreeDiffLimits::new(copy_limits, 16, 4096, 8192).unwrap();
        let execution_limits = super::super::WindowsContainerExecutionLimits::new(
            1024 * 1024,
            Duration::from_secs(10),
            8192,
            8192,
            copy_limits,
            diff_limits,
        )
        .unwrap();
        let request = super::super::WindowsContainerExecutionRequest::new(
            super::super::WindowsContainerShell::PowerShell,
            "Write-Output 'hello'",
            "",
            7,
            11,
            execution_limits,
        )
        .unwrap();
        let request_id = [9; 16];
        let mut client = BrokerPipeClient::connect_for_test(&name).unwrap();
        let mut on_output = |_: super::super::WindowsContainerOutputStream, _: &[u8]| {};
        let output_sink = super::super::WindowsContainerOutputSink::new(&request, &mut on_output);
        let mut before_shell_start = || Ok(());
        let mut shell_started = || {};
        let response = client
            .execute_with_workspace(
                &request,
                request_id,
                &workspace,
                &mut before_shell_start,
                &mut shell_started,
                output_sink,
            )
            .unwrap();

        assert!(matches!(
            response,
            client_exchange::BrokerPipeExecutionResponse::Failed(
                BrokerFailureCode::BackendUnavailable
            )
        ));
        assert_eq!(server.join().unwrap().unwrap(), (7, b"payload".to_vec()));
    }

    #[test]
    fn request_scoped_quota_failure_is_acknowledged_before_workspace_transfer() {
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
        let server = std::thread::spawn(move || -> io::Result<()> {
            let mut listener = BrokerPipeListener::create(&server_name, policy)?;
            let cancellation = BrokerPipeCancellation::new()?;
            ready_sender.send(()).unwrap();
            let connection = listener.accept(&cancellation)?;
            let request = match connection
                .read_frame(Duration::from_secs(5))?
                .decode_control()?
            {
                BrokerControlMessage::Execute { request } => request,
                _ => return Err(invalid_data("test client sent an invalid request")),
            };
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::Failure {
                    request_id: Some(request.request_id),
                    code: BrokerFailureCode::QuotaUnattested,
                })?,
                Duration::from_secs(5),
            )?;
            match connection
                .read_frame(Duration::from_secs(5))?
                .decode_control()?
            {
                BrokerControlMessage::ExecutionAcknowledged { request_id }
                    if request_id == request.request_id =>
                {
                    Ok(())
                }
                _ => Err(invalid_data(
                    "test client did not acknowledge the quota failure",
                )),
            }
        });
        ready_receiver.recv().unwrap();

        let workspace_temp = tempfile::tempdir().unwrap();
        std::fs::write(workspace_temp.path().join("payload.txt"), b"payload").unwrap();
        let workspace = super::super::WindowsContainerWorkspaceInput::new(
            crate::filesystem::DirectoryCapability::open(workspace_temp.path()).unwrap(),
        );
        let copy_limits =
            crate::filesystem::DirectoryTreeCopyLimits::new(16, 8, 4096, 8192).unwrap();
        let diff_limits =
            crate::filesystem::DirectoryTreeDiffLimits::new(copy_limits, 16, 4096, 8192).unwrap();
        let execution_limits = super::super::WindowsContainerExecutionLimits::new(
            1024 * 1024,
            Duration::from_secs(10),
            8192,
            8192,
            copy_limits,
            diff_limits,
        )
        .unwrap();
        let request = super::super::WindowsContainerExecutionRequest::new(
            super::super::WindowsContainerShell::PowerShell,
            "Write-Output 'hello'",
            "",
            7,
            11,
            execution_limits,
        )
        .unwrap();
        let mut client = BrokerPipeClient::connect_for_test(&name).unwrap();
        let mut on_output = |_: super::super::WindowsContainerOutputStream, _: &[u8]| {};
        let output_sink = super::super::WindowsContainerOutputSink::new(&request, &mut on_output);
        let mut start_requests = 0;
        let mut start_confirmations = 0;
        let mut before_shell_start = || {
            start_requests += 1;
            Ok(())
        };
        let mut shell_started = || start_confirmations += 1;
        let response = client
            .execute_with_workspace(
                &request,
                [13; 16],
                &workspace,
                &mut before_shell_start,
                &mut shell_started,
                output_sink,
            )
            .unwrap();

        assert!(matches!(
            response,
            client_exchange::BrokerPipeExecutionResponse::Failed(
                BrokerFailureCode::QuotaUnattested
            )
        ));
        assert_eq!(start_requests, 0);
        assert_eq!(start_confirmations, 0);
        server.join().unwrap().unwrap();
    }

    #[test]
    fn broker_client_reconstructs_output_and_returns_a_bound_review_owner() {
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
        let server = std::thread::spawn(move || -> io::Result<()> {
            let mut listener = BrokerPipeListener::create(&server_name, policy)?;
            let cancellation = BrokerPipeCancellation::new()?;
            ready_sender.send(()).unwrap();
            let connection = listener.accept(&cancellation)?;
            let request = match connection
                .read_frame(Duration::from_secs(5))?
                .decode_control()?
            {
                BrokerControlMessage::Execute { request } => request,
                _ => return Err(invalid_data("test client sent an invalid request")),
            };
            let request_id = request.request_id;
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::WorkspaceSnapshotRequested {
                    request_id,
                })?,
                Duration::from_secs(5),
            )?;

            let staging_temp = tempfile::tempdir()?;
            let staging = crate::filesystem::DirectoryCapability::open(staging_temp.path())?;
            let limits = request.validate()?;
            let cancellation = connection.cancellation();
            let (snapshot, summary) = super::super::workspace_transfer::receive_workspace_snapshot(
                staging,
                limits.workspace_copy(),
                || connection.read_frame(Duration::from_secs(5)),
                || cancellation.is_cancelled(),
            )?;
            let output_temp = tempfile::tempdir()?;
            let output = crate::filesystem::DirectoryCapability::open(output_temp.path())?;
            snapshot.copy_tree_to(&output, limits.workspace_copy(), || false)?;
            std::fs::write(output_temp.path().join("result.txt"), b"result")?;

            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::ExecutionStartRequested {
                    request_id,
                })?,
                Duration::from_secs(5),
            )?;
            if !matches!(
                connection
                    .read_frame(Duration::from_secs(5))?
                    .decode_control()?,
                BrokerControlMessage::ExecutionStartAuthorized {
                    request_id: authorized_id
                } if authorized_id == request_id
            ) {
                return Err(invalid_data("test client did not authorize Shell start"));
            }
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::ExecutionStarted { request_id })?,
                Duration::from_secs(5),
            )?;

            connection.write_frame(
                &BrokerFrame::new(BrokerFrameKind::StdoutChunk, b"hello".to_vec())?,
                Duration::from_secs(5),
            )?;
            super::super::output_transfer::send_output_snapshot(
                &output,
                &snapshot,
                limits.output_diff(),
                summary.byte_count,
                |frame| connection.write_frame(&frame, Duration::from_secs(5)),
                || cancellation.is_cancelled(),
            )?;
            connection.write_frame(
                &BrokerFrame::control(&BrokerControlMessage::ExecutionComplete {
                    request_id,
                    exit_code: 0,
                    stdout_truncated: false,
                    stderr_truncated: false,
                })?,
                Duration::from_secs(5),
            )?;
            if !matches!(
                connection
                    .read_frame(Duration::from_secs(5))?
                    .decode_control()?,
                BrokerControlMessage::ExecutionAcknowledged {
                    request_id: acknowledged_id
                } if acknowledged_id == request_id
            ) {
                return Err(invalid_data("test client did not acknowledge completion"));
            }
            Ok(())
        });
        ready_receiver.recv().unwrap();

        let workspace_temp = tempfile::tempdir().unwrap();
        std::fs::write(workspace_temp.path().join("payload.txt"), b"payload").unwrap();
        let workspace = super::super::WindowsContainerWorkspaceInput::new(
            crate::filesystem::DirectoryCapability::open(workspace_temp.path()).unwrap(),
        );
        let copy_limits =
            crate::filesystem::DirectoryTreeCopyLimits::new(16, 8, 4096, 8192).unwrap();
        let diff_limits =
            crate::filesystem::DirectoryTreeDiffLimits::new(copy_limits, 16, 4096, 8192).unwrap();
        let execution_limits = super::super::WindowsContainerExecutionLimits::new(
            1024 * 1024,
            Duration::from_secs(10),
            8192,
            8192,
            copy_limits,
            diff_limits,
        )
        .unwrap();
        let request = super::super::WindowsContainerExecutionRequest::new(
            super::super::WindowsContainerShell::PowerShell,
            "Write-Output 'hello'",
            "",
            7,
            11,
            execution_limits,
        )
        .unwrap();
        let request_id = [12; 16];
        let mut client = BrokerPipeClient::connect_for_test(&name).unwrap();
        let mut streamed_output = Vec::new();
        let mut on_output = |stream, bytes: &[u8]| {
            assert_eq!(stream, super::super::WindowsContainerOutputStream::Stdout);
            streamed_output.extend_from_slice(bytes);
        };
        let output_sink = super::super::WindowsContainerOutputSink::new(&request, &mut on_output);
        let mut start_authorizations = 0;
        let mut start_confirmations = 0;
        let mut before_shell_start = || {
            start_authorizations += 1;
            Ok(())
        };
        let mut shell_started = || start_confirmations += 1;
        let response = client.execute_with_workspace(
            &request,
            request_id,
            &workspace,
            &mut before_shell_start,
            &mut shell_started,
            output_sink,
        );
        let server_result = server.join().unwrap();
        assert!(
            server_result.is_ok(),
            "server exchange failed: {server_result:?}"
        );
        assert_eq!(start_authorizations, 1);
        assert_eq!(start_confirmations, 1);
        let response = response.unwrap();

        let client_exchange::BrokerPipeExecutionResponse::Completed {
            exit_code,
            captured_output,
            output_review,
        } = response
        else {
            panic!("the broker completed the requested output review")
        };
        assert_eq!(exit_code, 0);
        assert_eq!(captured_output.stdout(), b"hello");
        assert_eq!(streamed_output, b"hello");
        assert_eq!(output_review.manifest().changes.len(), 1);
        let change = &output_review.manifest().changes[0];
        assert_eq!(change.relative_path, Path::new("result.txt"));

        let destination_temp = tempfile::tempdir().unwrap();
        let destination =
            crate::filesystem::DirectoryCapability::open(destination_temp.path()).unwrap();
        output_review
            .promote_output_file_to(change, 7, 11, &destination, Path::new("result.txt"), || {
                false
            })
            .unwrap();
        assert_eq!(
            std::fs::read(destination_temp.path().join("result.txt")).unwrap(),
            b"result"
        );
        output_review.cleanup().unwrap();
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
            let mut listener = BrokerPipeListener::create(&server_name, policy).unwrap();
            let cancellation = BrokerPipeCancellation::new().unwrap();
            ready_sender.send(()).unwrap();
            let result = listener
                .accept(&cancellation)
                .and_then(|connection| connection.read_frame(Duration::from_millis(30)))
                .map(|_| ())
                .map_err(|error| error.kind());
            result_sender.send(result).unwrap();
        });
        ready_receiver.recv().unwrap();
        let client = BrokerPipeClient::connect_for_test(&name).unwrap();
        assert_eq!(
            result_receiver.recv().unwrap().unwrap_err(),
            io::ErrorKind::TimedOut
        );
        drop(client);
        server.join().unwrap();
    }

    #[test]
    fn named_pipe_accept_is_cancelled_by_the_service_signal() {
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
            let mut listener = BrokerPipeListener::create(&server_name, policy).unwrap();
            let cancellation = BrokerPipeCancellation::new().unwrap();
            ready_sender
                .send(cancellation.raw_handle() as usize)
                .unwrap();
            let result = listener
                .accept(&cancellation)
                .map(|_| ())
                .map_err(|error| error.kind());
            result_sender.send(result).unwrap();
        });
        let cancellation_handle = ready_receiver.recv().unwrap() as HANDLE;
        assert_ne!(unsafe { SetEvent(cancellation_handle) }, 0);
        assert_eq!(
            result_receiver.recv().unwrap().unwrap_err(),
            io::ErrorKind::Interrupted
        );
        server.join().unwrap();
    }

    #[test]
    fn client_cancellation_interrupts_a_pending_frame_read() {
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
        let (release_sender, release_receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut listener = BrokerPipeListener::create(&server_name, policy).unwrap();
            let cancellation = BrokerPipeCancellation::new().unwrap();
            ready_sender.send(()).unwrap();
            let connection = listener.accept(&cancellation).unwrap();
            let _ = release_receiver.recv();
            drop(connection);
        });
        ready_receiver.recv().unwrap();
        let cancellation = BrokerPipeCancellation::new().unwrap();
        let mut client =
            BrokerPipeClient::connect_for_test_with_cancellation(&name, cancellation.clone())
                .unwrap();
        let cancel_signal = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            cancellation.cancel().unwrap();
        });
        assert_eq!(
            client
                .read_frame(Duration::from_secs(5))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        cancel_signal.join().unwrap();
        drop(client);
        release_sender.send(()).unwrap();
        server.join().unwrap();
    }

    #[test]
    fn broker_connection_detects_client_disconnect_without_pipe_reads() {
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
        let (disconnect_sender, disconnect_receiver) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut listener = BrokerPipeListener::create(&server_name, policy).unwrap();
            let cancellation = BrokerPipeCancellation::new().unwrap();
            ready_sender.send(()).unwrap();
            let connection = listener.accept(&cancellation).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if connection.peer_disconnected() {
                    disconnect_sender.send(true).unwrap();
                    break;
                }
                if Instant::now() >= deadline {
                    disconnect_sender.send(false).unwrap();
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        ready_receiver.recv().unwrap();
        let client = BrokerPipeClient::connect_for_test(&name).unwrap();
        drop(client);

        assert!(disconnect_receiver.recv().unwrap());
        server.join().unwrap();
    }

    #[test]
    fn named_pipe_write_times_out_when_the_client_stops_reading() {
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
            let mut listener = BrokerPipeListener::create(&server_name, policy).unwrap();
            let cancellation = BrokerPipeCancellation::new().unwrap();
            ready_sender.send(()).unwrap();
            let result = listener
                .accept(&cancellation)
                .and_then(|connection| {
                    let frame = BrokerFrame::new(BrokerFrameKind::StdoutChunk, vec![0; 64 * 1024])?;
                    connection.write_frame(&frame, Duration::from_millis(50))
                })
                .map_err(|error| error.kind());
            result_sender.send(result).unwrap();
        });
        ready_receiver.recv().unwrap();
        let client = BrokerPipeClient::connect_for_test(&name).unwrap();
        assert_eq!(
            result_receiver.recv().unwrap().unwrap_err(),
            io::ErrorKind::TimedOut
        );
        drop(client);
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
            let cancellation = BrokerPipeCancellation::new().unwrap();
            match listener.accept(&cancellation) {
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
