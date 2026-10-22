//! Windows SID, token, ACL, and SCM identity operations for the broker pipe.

mod wts_identity;

use super::super::BROKER_SERVICE_NAME;
use super::{
    BrokerPeerIdentity, BrokerPeerPolicy, OwnedHandle, invalid_data, invalid_input,
    permission_denied, wide_null,
};
use std::io;
use std::mem::{offset_of, size_of};
use std::ptr::{null, null_mut};
use windows_sys::Wdk::Storage::FileSystem::RtlCreateServiceSid;
use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_NONE_MAPPED, GetLastError, HANDLE, LocalFree,
    STATUS_BUFFER_TOO_SMALL, STATUS_SUCCESS, UNICODE_STRING,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetLengthSid, GetTokenInformation, IsValidSid, LookupAccountNameW, RevertToSelf,
    SID_AND_ATTRIBUTES, SID_NAME_USE, TOKEN_GROUPS, TOKEN_QUERY, TOKEN_USER, TokenGroups,
    TokenSessionId, TokenUser,
};
use windows_sys::Win32::System::Pipes::{
    GetNamedPipeClientProcessId, GetNamedPipeClientSessionId, GetNamedPipeServerProcessId,
    GetNamedPipeServerSessionId, ImpersonateNamedPipeClient,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
    SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
};
use windows_sys::Win32::System::SystemServices::SE_GROUP_LOGON_ID;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, OpenProcessToken, OpenThreadToken,
};

const MAX_TOKEN_INFORMATION_BYTES: u32 = 64 * 1024;
const MAX_TOKEN_GROUPS: usize = 1_024;
pub(super) const MAX_SID_BYTES: usize = 68;
const SE_GROUP_LOGON_ID_MASK: u32 = SE_GROUP_LOGON_ID as u32;

pub(super) struct PipeSecurityDescriptor {
    descriptor: *mut core::ffi::c_void,
}

impl PipeSecurityDescriptor {
    pub(super) fn for_policy(policy: &BrokerPeerPolicy) -> io::Result<Self> {
        let service_sid = sid_to_string(&policy.service_sid)?;
        let logon_sid = sid_to_string(&policy.logon_sid)?;
        // Protected DACL: only the broker service and this logon session can
        // open the pipe. It intentionally omits Everyone, Authenticated Users,
        // administrators, and anonymous access.
        let sddl = format!("D:P(A;;GA;;;{service_sid})(A;;GRGW;;;{logon_sid})");
        let sddl = wide_null(&sddl);
        let mut descriptor = null_mut();
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        };
        if converted == 0 || descriptor.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }

    pub(super) fn as_ptr(&self) -> *mut core::ffi::c_void {
        self.descriptor
    }
}

impl Drop for PipeSecurityDescriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(self.descriptor) };
    }
}

struct OwnedServiceHandle(windows_sys::Win32::System::Services::SC_HANDLE);

impl Drop for OwnedServiceHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

pub(super) fn impersonate_and_read_peer(handle: HANDLE) -> io::Result<BrokerPeerIdentity> {
    if unsafe { ImpersonateNamedPipeClient(handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let revert = RevertToSelfGuard { active: true };
    let identity = (|| {
        let mut token = null_mut();
        if unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle(token);
        let mut process_id = 0;
        let mut pipe_session_id = 0;
        if unsafe { GetNamedPipeClientProcessId(handle, &mut process_id) } == 0
            || unsafe { GetNamedPipeClientSessionId(handle, &mut pipe_session_id) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        token_identity(token.0, process_id, pipe_session_id)
    })();
    // Do not return to the service loop until its thread token is restored.
    // If Windows cannot revert it, continuing could run privileged work under
    // the client identity, so the process must fail closed.
    revert.revert();
    identity
}

struct RevertToSelfGuard {
    active: bool,
}

impl RevertToSelfGuard {
    fn revert(mut self) {
        if unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
        self.active = false;
    }
}

impl Drop for RevertToSelfGuard {
    fn drop(&mut self) {
        if self.active && unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
    }
}

pub(super) fn current_process_identity() -> io::Result<BrokerPeerIdentity> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    let process_id = unsafe { GetCurrentProcessId() };
    let mut session_id = 0;
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenSessionId,
            (&mut session_id as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut returned,
        )
    } == 0
        || returned != size_of::<u32>() as u32
    {
        return Err(io::Error::last_os_error());
    }
    token_identity(token.0, process_id, session_id)
}

fn token_identity(
    token: HANDLE,
    process_id: u32,
    pipe_session_id: u32,
) -> io::Result<BrokerPeerIdentity> {
    let user_sid = token_user_sid(token)?;
    let logon_sid = token_logon_sid(token)?;
    let token_session_id = token_session_id(token)?;
    if process_id == 0 || token_session_id == 0 || token_session_id != pipe_session_id {
        return Err(permission_denied("broker peer session identity is invalid"));
    }
    Ok(BrokerPeerIdentity {
        process_id,
        session_id: token_session_id,
        user_sid,
        logon_sid,
    })
}

fn token_user_sid(token: HANDLE) -> io::Result<Vec<u8>> {
    let (buffer, byte_length) = token_information(token, TokenUser)?;
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    copy_sid_from_buffer(user.User.Sid, &buffer, byte_length)
}

fn token_logon_sid(token: HANDLE) -> io::Result<Vec<u8>> {
    let (buffer, byte_length) = token_information(token, TokenGroups)?;
    if byte_length < offset_of!(TOKEN_GROUPS, Groups) {
        return Err(invalid_data("client token groups are malformed"));
    }
    let groups = unsafe { &*(buffer.as_ptr().cast::<TOKEN_GROUPS>()) };
    let count = groups.GroupCount as usize;
    let required = offset_of!(TOKEN_GROUPS, Groups)
        .checked_add(
            count
                .checked_mul(size_of::<SID_AND_ATTRIBUTES>())
                .ok_or_else(|| invalid_data("client token group count is outside its limit"))?,
        )
        .ok_or_else(|| invalid_data("client token group size overflowed"))?;
    if count == 0 || count > MAX_TOKEN_GROUPS || required > byte_length {
        return Err(invalid_data("client token groups are outside their limit"));
    }
    let first_group = std::ptr::addr_of!(groups.Groups).cast::<SID_AND_ATTRIBUTES>();
    let entries = unsafe { std::slice::from_raw_parts(first_group, count) };
    let mut logon_sid = None;
    for group in entries {
        if group.Attributes & SE_GROUP_LOGON_ID_MASK == SE_GROUP_LOGON_ID_MASK {
            if logon_sid.is_some() {
                return Err(invalid_data("client token has multiple logon SIDs"));
            }
            logon_sid = Some(copy_sid_from_buffer(group.Sid, &buffer, byte_length)?);
        }
    }
    logon_sid.ok_or_else(|| permission_denied("client token has no logon SID"))
}

fn token_session_id(token: HANDLE) -> io::Result<u32> {
    let mut session_id = 0;
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token,
            TokenSessionId,
            (&mut session_id as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut returned,
        )
    } == 0
        || returned != size_of::<u32>() as u32
    {
        return Err(io::Error::last_os_error());
    }
    Ok(session_id)
}

fn token_information(token: HANDLE, class: i32) -> io::Result<(Vec<usize>, usize)> {
    let mut required = 0;
    let queried = unsafe { GetTokenInformation(token, class, null_mut(), 0, &mut required) };
    if queried != 0
        || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER
        || required == 0
        || required > MAX_TOKEN_INFORMATION_BYTES
    {
        return Err(io::Error::last_os_error());
    }
    let word_count = (required as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; word_count];
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            required,
            &mut returned,
        )
    } == 0
        || returned == 0
        || returned > required
    {
        return Err(io::Error::last_os_error());
    }
    Ok((buffer, returned as usize))
}

fn copy_sid_from_buffer(
    sid: *mut core::ffi::c_void,
    buffer: &[usize],
    byte_length: usize,
) -> io::Result<Vec<u8>> {
    let base = buffer.as_ptr() as usize;
    let end = base
        .checked_add(byte_length)
        .ok_or_else(|| invalid_data("client token buffer range overflowed"))?;
    let address = sid as usize;
    if sid.is_null() || address < base || address >= end {
        return Err(invalid_data("client SID is outside its token buffer"));
    }
    let available = end - address;
    if available < 8 || unsafe { IsValidSid(sid) } == 0 {
        return Err(invalid_data("client token SID is invalid"));
    }
    let length = unsafe { GetLengthSid(sid) } as usize;
    if !(8..=MAX_SID_BYTES).contains(&length) || length > available {
        return Err(invalid_data("client token SID length is invalid"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), length) };
    Ok(bytes.to_vec())
}

pub(super) fn validate_sid_bytes(sid: &[u8]) -> io::Result<()> {
    if sid.len() < 8 || sid.len() > MAX_SID_BYTES || sid[0] != 1 || sid[1] > 15 {
        return Err(invalid_input("broker peer SID is invalid"));
    }
    let expected_length = 8 + sid[1] as usize * size_of::<u32>();
    if sid.len() != expected_length || unsafe { IsValidSid(sid.as_ptr().cast_mut().cast()) } == 0 {
        return Err(invalid_input("broker peer SID is invalid"));
    }
    Ok(())
}

fn sid_to_string(sid: &[u8]) -> io::Result<String> {
    validate_sid_bytes(sid)?;
    let mut output = null_mut();
    if unsafe { ConvertSidToStringSidW(sid.as_ptr().cast_mut().cast(), &mut output) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let _allocated = LocalString(output);
    let mut length = 0;
    while length < 256 && unsafe { *output.add(length) } != 0 {
        length += 1;
    }
    if length == 256 {
        return Err(invalid_data("formatted broker SID is too long"));
    }
    let value = unsafe { std::slice::from_raw_parts(output, length) };
    String::from_utf16(value).map_err(|_| invalid_data("formatted broker SID is invalid"))
}

pub(super) struct LocalString(pub(super) *mut u16);

impl Drop for LocalString {
    fn drop(&mut self) {
        unsafe { LocalFree(self.0.cast()) };
    }
}

/// Identity snapshot read from the unique active WTS user token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActiveSessionIdentity {
    pub(super) session_id: u32,
    pub(super) user_sid: Vec<u8>,
    pub(super) logon_sid: Vec<u8>,
}

pub(super) fn query_unique_active_session() -> io::Result<ActiveSessionIdentity> {
    wts_identity::query_unique_active_session()
}

pub(super) fn current_broker_service_sid() -> io::Result<Vec<u8>> {
    let sid = broker_service_sid()?;
    verify_current_service_identity(&sid)?;
    Ok(sid)
}

pub(super) fn broker_service_sid() -> io::Result<Vec<u8>> {
    let account = wide_null(&format!(r"NT SERVICE\{BROKER_SERVICE_NAME}"));
    let mut sid_length = 0;
    let mut domain_length = 0;
    let mut account_use: SID_NAME_USE = 0;
    let lookup_succeeded = unsafe {
        LookupAccountNameW(
            null(),
            account.as_ptr(),
            null_mut(),
            &mut sid_length,
            null_mut(),
            &mut domain_length,
            &mut account_use,
        )
    } != 0;
    let lookup_error = if lookup_succeeded {
        0
    } else {
        unsafe { GetLastError() }
    };
    if !lookup_succeeded {
        if lookup_error == ERROR_NONE_MAPPED {
            // Some Windows builds fail to resolve the documented NT SERVICE
            // account name even after SCM has configured the service SID.
            // Derive the same OS-owned SID directly rather than weakening the
            // service ACL or falling back to LocalSystem/Administrators.
            return broker_service_sid_from_service_name();
        }
        if lookup_error != ERROR_INSUFFICIENT_BUFFER {
            return Err(io::Error::from_raw_os_error(lookup_error as i32));
        }
    } else {
        return Err(invalid_data(
            "service SID lookup succeeded without output buffers",
        ));
    }
    if sid_length == 0 || sid_length as usize > MAX_SID_BYTES || domain_length > 256 {
        return Err(invalid_data("service SID lookup returned an invalid size"));
    }

    let mut sid = vec![0_u8; sid_length as usize];
    let mut domain = vec![0_u16; domain_length as usize];
    if unsafe {
        LookupAccountNameW(
            null(),
            account.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_length,
            domain.as_mut_ptr(),
            &mut domain_length,
            &mut account_use,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    sid.truncate(sid_length as usize);
    validate_sid_bytes(&sid)?;
    Ok(sid)
}

fn broker_service_sid_from_service_name() -> io::Result<Vec<u8>> {
    let mut service_name = wide_null(BROKER_SERVICE_NAME);
    let service_name_length = service_name
        .len()
        .checked_sub(1)
        .and_then(|length| length.checked_mul(size_of::<u16>()))
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| invalid_input("broker service name is too long"))?;
    let counted_service_name = UNICODE_STRING {
        Length: service_name_length,
        MaximumLength: service_name_length,
        Buffer: service_name.as_mut_ptr(),
    };

    let mut sid_length = 0;
    let status = unsafe { RtlCreateServiceSid(&counted_service_name, null_mut(), &mut sid_length) };
    if status != STATUS_BUFFER_TOO_SMALL {
        return Err(nt_status_error(status));
    }
    if !(8..=MAX_SID_BYTES).contains(&(sid_length as usize)) {
        return Err(invalid_data(
            "service SID size is outside the supported limit",
        ));
    }

    let mut sid = vec![0_u8; sid_length as usize];
    let status = unsafe {
        RtlCreateServiceSid(
            &counted_service_name,
            sid.as_mut_ptr().cast(),
            &mut sid_length,
        )
    };
    if status != STATUS_SUCCESS {
        return Err(nt_status_error(status));
    }
    sid.truncate(sid_length as usize);
    validate_sid_bytes(&sid)?;
    Ok(sid)
}

fn nt_status_error(status: i32) -> io::Error {
    io::Error::other(format!(
        "RtlCreateServiceSid failed with NTSTATUS 0x{:08X}",
        status as u32
    ))
}

pub(super) fn broker_service_sid_string() -> io::Result<String> {
    sid_to_string(&broker_service_sid()?)
}

pub(super) fn verify_pipe_server(handle: HANDLE) -> io::Result<()> {
    let mut server_pid = 0;
    let mut server_session = u32::MAX;
    if unsafe { GetNamedPipeServerProcessId(handle, &mut server_pid) } == 0
        || unsafe { GetNamedPipeServerSessionId(handle, &mut server_session) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if server_pid == 0 || server_session != 0 || scm_service_process_id()? != server_pid {
        return Err(permission_denied(
            "named-pipe server is not the broker service",
        ));
    }
    Ok(())
}

pub(super) fn verify_current_service_identity(service_sid: &[u8]) -> io::Result<()> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    if token_session_id(token.0)? != 0 || !token_has_sid(token.0, service_sid)? {
        return Err(permission_denied(
            "broker listener is not running as its service identity",
        ));
    }
    Ok(())
}

fn token_has_sid(token: HANDLE, sid: &[u8]) -> io::Result<bool> {
    validate_sid_bytes(sid)?;
    let (buffer, byte_length) = token_information(token, TokenUser)?;
    let user = unsafe { &*(buffer.as_ptr().cast::<TOKEN_USER>()) };
    if copy_sid_from_buffer(user.User.Sid, &buffer, byte_length)? == sid {
        return Ok(true);
    }
    let (buffer, byte_length) = token_information(token, TokenGroups)?;
    if byte_length < offset_of!(TOKEN_GROUPS, Groups) {
        return Err(invalid_data("process token groups are malformed"));
    }
    let groups = unsafe { &*(buffer.as_ptr().cast::<TOKEN_GROUPS>()) };
    let count = groups.GroupCount as usize;
    let required = offset_of!(TOKEN_GROUPS, Groups)
        .checked_add(
            count
                .checked_mul(size_of::<SID_AND_ATTRIBUTES>())
                .ok_or_else(|| invalid_data("process token group count is outside its limit"))?,
        )
        .ok_or_else(|| invalid_data("process token group size overflowed"))?;
    if count > MAX_TOKEN_GROUPS || required > byte_length {
        return Err(invalid_data("process token groups are outside their limit"));
    }
    let first_group = std::ptr::addr_of!(groups.Groups).cast::<SID_AND_ATTRIBUTES>();
    for group in unsafe { std::slice::from_raw_parts(first_group, count) } {
        if copy_sid_from_buffer(group.Sid, &buffer, byte_length)? == sid {
            return Ok(true);
        }
    }
    Ok(false)
}

fn scm_service_process_id() -> io::Result<u32> {
    let manager = unsafe { OpenSCManagerW(null(), null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(io::Error::last_os_error());
    }
    let manager = OwnedServiceHandle(manager);
    let service_name = wide_null(BROKER_SERVICE_NAME);
    let service = unsafe { OpenServiceW(manager.0, service_name.as_ptr(), SERVICE_QUERY_STATUS) };
    if service.is_null() {
        return Err(io::Error::last_os_error());
    }
    let service = OwnedServiceHandle(service);
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut returned = 0;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut returned,
        )
    } == 0
        || returned < size_of::<SERVICE_STATUS_PROCESS>() as u32
        || status.dwCurrentState != SERVICE_RUNNING
        || status.dwProcessId == 0
    {
        return Err(permission_denied("broker service is not running"));
    }
    Ok(status.dwProcessId)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_sid_fallback_matches_the_scm_service_sid() {
        let sid = broker_service_sid_from_service_name().expect("service SID should be derived");
        assert_eq!(
            sid_to_string(&sid).expect("service SID should stringify"),
            "S-1-5-80-67550567-2145346782-217598459-3553917341-4058129405"
        );
    }
}
