use std::{
    ffi::c_void,
    io::{self, Write},
    ptr,
    time::{Duration, Instant},
};
use windows_sys::Win32::Networking::WinHttp::*;

struct Handle(*mut c_void);
impl Handle {
    fn new(handle: *mut c_void) -> io::Result<Self> {
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(handle))
        }
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub fn get(url: &str, output: &mut dyn Write, limit: usize) -> io::Result<()> {
    let began = Instant::now();
    let url = wide(url);
    // WinHTTP owns all HTTP/TLS handling. URL_COMPONENTS borrows slices of this UTF-16 URL.
    unsafe {
        let mut parts: URL_COMPONENTS = std::mem::zeroed();
        parts.dwStructSize = std::mem::size_of_val(&parts) as u32;
        parts.dwHostNameLength = u32::MAX;
        parts.dwUrlPathLength = u32::MAX;
        parts.dwExtraInfoLength = u32::MAX;
        check(WinHttpCrackUrl(
            url.as_ptr(),
            (url.len() - 1) as u32,
            0,
            &mut parts,
        ))?;
        if parts.dwHostNameLength == 0 || parts.lpszHostName.is_null() {
            return Err(io::Error::other("missing update host"));
        }
        let host = std::slice::from_raw_parts(parts.lpszHostName, parts.dwHostNameLength as usize);
        let mut host = host.to_vec();
        host.push(0);
        let mut path = Vec::new();
        if parts.dwUrlPathLength > 0 {
            path.extend_from_slice(std::slice::from_raw_parts(
                parts.lpszUrlPath,
                parts.dwUrlPathLength as usize,
            ));
        }
        if path.is_empty() {
            path.push('/' as u16);
        }
        if parts.dwExtraInfoLength > 0 {
            path.extend_from_slice(std::slice::from_raw_parts(
                parts.lpszExtraInfo,
                parts.dwExtraInfoLength as usize,
            ));
        }
        path.push(0);
        let agent = wide(concat!("mcp-lazy-proxy/", env!("CARGO_PKG_VERSION")));
        let session = Handle::new(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            ptr::null(),
            ptr::null(),
            0,
        ))?;
        check(WinHttpSetTimeouts(session.0, 3000, 3000, 3000, 5000))?;
        let connection = Handle::new(WinHttpConnect(session.0, host.as_ptr(), parts.nPort, 0))?;
        let flags = if parts.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS {
            WINHTTP_FLAG_SECURE
        } else {
            0
        };
        let verb = wide("GET");
        let request = Handle::new(WinHttpOpenRequest(
            connection.0,
            verb.as_ptr(),
            path.as_ptr(),
            ptr::null(),
            ptr::null(),
            ptr::null(),
            flags,
        ))?;
        let headers =
            wide("Accept: application/vnd.github+json\r\nX-GitHub-Api-Version: 2022-11-28\r\n");
        check(WinHttpSendRequest(
            request.0,
            headers.as_ptr(),
            (headers.len() - 1) as u32,
            ptr::null(),
            0,
            0,
            0,
        ))?;
        check(WinHttpReceiveResponse(request.0, ptr::null_mut()))?;
        let mut status = 0u32;
        let mut size = std::mem::size_of_val(&status) as u32;
        check(WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            ptr::null(),
            (&mut status as *mut u32).cast(),
            &mut size,
            ptr::null_mut(),
        ))?;
        if status != 200 {
            return Err(io::Error::other("update HTTP request failed"));
        }
        let mut total = 0usize;
        loop {
            if began.elapsed() > Duration::from_secs(20) {
                return Err(io::Error::other("update download timed out"));
            }
            let mut buffer = [0u8; 32768];
            let mut read = 0u32;
            check(WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            ))?;
            if read == 0 {
                return Ok(());
            }
            total += read as usize;
            if total > limit {
                return Err(io::Error::other("update response too large"));
            }
            output.write_all(&buffer[..read as usize])?;
        }
    }
}
