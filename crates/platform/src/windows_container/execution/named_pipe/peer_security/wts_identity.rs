//! WTS-backed selection of the sole interactive session authorized for the broker pipe.

use super::{ActiveSessionIdentity, token_logon_sid, token_session_id, token_user_sid};
use std::io;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::RemoteDesktop::{
    WTS_CURRENT_SERVER_HANDLE, WTS_SESSION_INFOW, WTSActive, WTSEnumerateSessionsW, WTSFreeMemory,
    WTSQueryUserToken,
};

const MAX_WTS_SESSIONS: u32 = 256;

pub(super) fn query_unique_active_session() -> io::Result<ActiveSessionIdentity> {
    let session_id = enumerate_unique_active_session_id()?;
    let mut token = null_mut();
    if unsafe { WTSQueryUserToken(session_id, &mut token) } == 0 || token.is_null() {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedToken(token);
    if token_session_id(token.0)? != session_id {
        return Err(permission_denied(
            "WTS user token does not match the active session",
        ));
    }
    Ok(ActiveSessionIdentity {
        session_id,
        user_sid: token_user_sid(token.0)?,
        logon_sid: token_logon_sid(token.0)?,
    })
}

fn enumerate_unique_active_session_id() -> io::Result<u32> {
    let mut sessions = null_mut();
    let mut count = 0;
    if unsafe { WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut sessions, &mut count) }
        == 0
    {
        return Err(io::Error::last_os_error());
    }
    if sessions.is_null() {
        return if count == 0 {
            select_unique_active_session_id(&[])
        } else {
            Err(invalid_data("WTS returned a null session list"))
        };
    }
    let _sessions = WtsMemory(sessions.cast());
    if count > MAX_WTS_SESSIONS {
        return Err(invalid_data("WTS session list exceeds its limit"));
    }
    let sessions = unsafe { std::slice::from_raw_parts(sessions, count as usize) };
    select_unique_active_session_id(sessions)
}

fn select_unique_active_session_id(sessions: &[WTS_SESSION_INFOW]) -> io::Result<u32> {
    let mut active_session_id = None;
    for session in sessions {
        if session.State != WTSActive || session.SessionId == 0 {
            continue;
        }
        if active_session_id.replace(session.SessionId).is_some() {
            return Err(permission_denied(
                "multiple active interactive sessions are not supported",
            ));
        }
    }
    active_session_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no active interactive user session is available",
        )
    })
}

struct WtsMemory(*mut core::ffi::c_void);

impl Drop for WtsMemory {
    fn drop(&mut self) {
        unsafe { WTSFreeMemory(self.0) };
    }
}

struct OwnedToken(HANDLE);

impl Drop for OwnedToken {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::RemoteDesktop::{WTS_SESSION_INFOW, WTSDisconnected};

    fn session(session_id: u32, state: i32) -> WTS_SESSION_INFOW {
        WTS_SESSION_INFOW {
            SessionId: session_id,
            pWinStationName: null_mut(),
            State: state,
        }
    }

    #[test]
    fn selects_the_only_active_nonzero_session() {
        let sessions = [
            session(0, WTSActive),
            session(1, WTSDisconnected),
            session(7, WTSActive),
        ];
        assert_eq!(select_unique_active_session_id(&sessions).unwrap(), 7);
    }

    #[test]
    fn rejects_missing_or_ambiguous_active_sessions() {
        assert_eq!(
            select_unique_active_session_id(&[session(0, WTSActive)])
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            select_unique_active_session_id(&[session(1, WTSActive), session(2, WTSActive),])
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
