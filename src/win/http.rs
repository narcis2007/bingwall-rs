//! HTTPS GET through WinHTTP. It uses the system certificate store and proxy
//! settings, so no TLS code is linked into the program.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

use super::wide;

type Handle = *mut c_void;

const RESOLVE_MS: i32 = 10_000;
const CONNECT_MS: i32 = 10_000;
const SEND_MS: i32 = 15_000;
/// Longest wait for one chunk of the body.
const RECEIVE_MS: i32 = 15_000;

fn fail(what: &str) -> String {
    // SAFETY: GetLastError has no preconditions. It is called right after the failed call.
    let code = unsafe { GetLastError() };
    let why = match code {
        12002 => "timed out".to_string(),
        12007 => "host name not found".to_string(),
        12029 | 12030 => "cannot connect".to_string(),
        12175 | 12157 => "TLS error".to_string(),
        _ => format!("Windows error {code}"),
    };
    format!("{what}: {why}")
}

/// One HTTPS session and connection to one host. Requests on it share the connection.
pub struct Client {
    session: Handle,
    connect: Handle,
}

impl Client {
    pub fn new(host: &str) -> Result<Client, String> {
        let agent = wide("bingwall/0.1");
        let host_w = wide(host);
        // SAFETY: all pointers are NUL-terminated buffers that outlive the calls.
        // Null is allowed for the proxy name and bypass list with AUTOMATIC_PROXY.
        unsafe {
            let session = WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                0,
            );
            if session.is_null() {
                return Err(fail("network start"));
            }
            WinHttpSetTimeouts(session, RESOLVE_MS, CONNECT_MS, SEND_MS, RECEIVE_MS);
            let connect = WinHttpConnect(session, host_w.as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0);
            if connect.is_null() {
                let e = fail("connect");
                WinHttpCloseHandle(session);
                return Err(e);
            }
            Ok(Client { session, connect })
        }
    }

    /// Send a GET request and read the status line and headers. The body is read
    /// with `Response::read`. `total` limits the whole request, body included.
    pub fn get(&self, path: &str, total: Duration) -> Result<Response<'_>, String> {
        let deadline = Instant::now() + total;
        let verb = wide("GET");
        let path_w = wide(path);
        // SAFETY: `self.connect` is a live handle; the strings are NUL-terminated
        // and outlive the call; null is allowed for version, referrer and accept types.
        let req = unsafe {
            WinHttpOpenRequest(
                self.connect,
                verb.as_ptr(),
                path_w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            )
        };
        if req.is_null() {
            return Err(fail("request"));
        }
        // From here on, dropping `resp` closes the request handle.
        let mut resp = Response { req, status: 0, deadline, _client: PhantomData };
        // SAFETY: `req` is a live request handle. No headers and no body are sent.
        let sent = unsafe { WinHttpSendRequest(req, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) };
        if sent == 0 {
            return Err(fail("send"));
        }
        // SAFETY: `req` is live; the reserved argument must be null.
        if unsafe { WinHttpReceiveResponse(req, std::ptr::null_mut()) } == 0 {
            return Err(fail("receive"));
        }
        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        // SAFETY: with the NUMBER flag the result is a u32 written to `status`;
        // `len` holds its size. Name and index pointers may be null.
        let ok = unsafe {
            WinHttpQueryHeaders(
                req,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                std::ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut len,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(fail("status"));
        }
        resp.status = status;
        Ok(resp)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // SAFETY: both handles are live and owned by this value. The connection
        // is closed before its session.
        unsafe {
            WinHttpCloseHandle(self.connect);
            WinHttpCloseHandle(self.session);
        }
    }
}

/// A reply whose headers are read. Borrows the `Client` so the request is closed before the connection.
pub struct Response<'a> {
    req: Handle,
    pub status: u32,
    deadline: Instant,
    _client: PhantomData<&'a Client>,
}

impl Response<'_> {
    /// Read up to `buf.len()` bytes of the body. `Ok(0)` is the end of the body.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if Instant::now() > self.deadline {
            return Err("download timed out".to_string());
        }
        let want = buf.len().min(u32::MAX as usize) as u32;
        let mut got: u32 = 0;
        // SAFETY: `req` is live; `buf` is valid for `want` bytes of writing and
        // `got` is a valid out-pointer.
        let ok = unsafe { WinHttpReadData(self.req, buf.as_mut_ptr().cast(), want, &mut got) };
        if ok == 0 {
            return Err(fail("read"));
        }
        Ok(got as usize)
    }
}

impl Drop for Response<'_> {
    fn drop(&mut self) {
        // SAFETY: `req` is a live request handle owned by this value.
        unsafe { WinHttpCloseHandle(self.req) };
    }
}
