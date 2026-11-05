//! Windows-only security boundary. ACLs name only the current token's user SID.
use crate::{EndpointDescriptor, Error, Result};
use std::{
    ffi::c_void,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::Path,
};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    System::{Pipes::*, Threading::*},
};

fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}
pub fn is_pipe_active(name: &str) -> Result<bool> {
    let name = wide(std::ffi::OsStr::new(name));
    unsafe {
        if WaitNamedPipeW(name.as_ptr(), 1) != 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(2) => Ok(false),
            Some(121) | Some(231) => Ok(true),
            _ => Err(Error::Io(error)),
        }
    }
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
fn private_descriptor() -> Result<Descriptor> {
    unsafe {
        let mut token = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut size);
        // usize allocation guarantees TOKEN_USER alignment.
        let mut buffer = vec![
            0usize;
            (size as usize + std::mem::size_of::<usize>() - 1)
                / std::mem::size_of::<usize>()
        ];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid_string = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_string) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut len = 0;
        while *sid_string.add(len) != 0 {
            len += 1;
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(sid_string, len));
        LocalFree(sid_string.cast());
        let sddl = wide(std::ffi::OsStr::new(&format!("D:P(A;OICI;GA;;;{sid})")));
        let mut descriptor = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Descriptor(descriptor))
    }
}
pub fn protect_path(path: &Path) -> Result<()> {
    let descriptor = private_descriptor()?;
    unsafe {
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = std::ptr::null_mut();
        if GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) == 0
            || present == 0
            || acl.is_null()
        {
            return Err(Error::Denied);
        }
        let name = wide(path.as_os_str());
        let status = SetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null_mut(),
        );
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status as i32).into());
        }
    }
    Ok(())
}
/// Keep the first handle alive for the entire runtime; subsequent instances do
/// not claim first-instance ownership. A competing installation fails closed.
pub fn create_pipe(name: &str, first: bool) -> Result<NamedPipeServer> {
    let descriptor = private_descriptor()?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(9);
    unsafe {
        options
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            )
            .map_err(Error::Io)
    }
}
pub fn verify_server(pipe: &NamedPipeClient, expected: &Path) -> Result<()> {
    unsafe {
        let mut pid = 0;
        if GetNamedPipeServerProcessId(pipe.as_raw_handle(), &mut pid) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut buffer = vec![0u16; 32768];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(process);
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let actual = std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..size as usize]))
            .canonicalize()?;
        if actual != expected.canonicalize()? {
            return Err(Error::Installation);
        }
        Ok(())
    }
}
pub async fn connect(descriptor: &EndpointDescriptor, bridge: &Path) -> Result<NamedPipeClient> {
    descriptor.validate_for_bridge(bridge)?;
    // No pipe discovery and no startup of another Lexena process.
    connect_pipe(
        &descriptor.pipe_name,
        &descriptor.server_path,
        std::time::Duration::from_secs(5),
    )
    .await
}
async fn connect_pipe(
    name: &str,
    expected_server: &Path,
    deadline: std::time::Duration,
) -> Result<NamedPipeClient> {
    // A connected instance can briefly be the only instance while the listener
    // is replaced. Retry only Windows' explicit busy indication, against the
    // same descriptor/name, under one total deadline. Missing/denied endpoints
    // and an incompatible server identity still fail immediately.
    let pipe = tokio::time::timeout(deadline, async {
        loop {
            match ClientOptions::new().open(name) {
                Ok(pipe) => return Ok::<_, Error>(pipe),
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                Err(error) => return Err(Error::Io(error)),
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout)??;
    verify_server(&pipe, expected_server)?;
    Ok(pipe)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn busy_pipe_waits_for_replacement_then_verifies_real_server_image() {
        let name = format!(r"\\.\pipe\lexena-mcp-busy-{}", uuid::Uuid::new_v4());
        let occupied = create_pipe(&name, true).unwrap();
        let first = ClientOptions::new().open(&name).unwrap();
        occupied.connect().await.unwrap();
        assert_eq!(
            ClientOptions::new().open(&name).unwrap_err().raw_os_error(),
            Some(ERROR_PIPE_BUSY as i32)
        );
        let target = name.clone();
        let expected = std::env::current_exe().unwrap();
        let connecting = tokio::spawn(async move {
            connect_pipe(&target, &expected, std::time::Duration::from_secs(5)).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(
            !connecting.is_finished(),
            "busy listener replacement must not fail immediately"
        );
        let replacement = create_pipe(&name, false).unwrap();
        replacement.connect().await.unwrap();
        let second = tokio::time::timeout(std::time::Duration::from_secs(2), connecting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        verify_server(&second, &std::env::current_exe().unwrap()).unwrap();
        let wrong =
            std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("System32/cmd.exe");
        assert!(matches!(
            verify_server(&second, &wrong),
            Err(Error::Installation)
        ));
        drop((second, replacement, first, occupied));
    }
    #[tokio::test]
    async fn busy_wait_has_total_deadline_and_missing_pipe_is_not_retried() {
        let name = format!(r"\\.\pipe\lexena-mcp-deadline-{}", uuid::Uuid::new_v4());
        let expected = std::env::current_exe().unwrap();
        assert!(
            matches!(tokio::time::timeout(std::time::Duration::from_millis(200),connect_pipe(&name,&expected,std::time::Duration::from_secs(5))).await.unwrap(),Err(Error::Io(error)) if error.raw_os_error()==Some(2))
        );
        let server = create_pipe(&name, true).unwrap();
        let client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        assert!(matches!(
            connect_pipe(&name, &expected, std::time::Duration::from_millis(40)).await,
            Err(Error::Timeout)
        ));
        drop((client, server));
    }
    #[tokio::test]
    async fn first_instance_excludes_second_installation_and_pid_spoof_is_denied() {
        let name = format!(r"\\.\pipe\lexena-mcp-test-{}", uuid::Uuid::new_v4());
        let pipe = create_pipe(&name, true).unwrap();
        assert!(create_pipe(&name, true).is_err());
        let client = ClientOptions::new().open(&name).unwrap();
        pipe.connect().await.unwrap();
        verify_server(&client, &std::env::current_exe().unwrap()).unwrap();
        let other =
            std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap()).join("System32/cmd.exe");
        assert!(verify_server(&client, &other).is_err());
    }
    #[tokio::test]
    async fn named_pipe_authenticated_bytes_and_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let installation = uuid::Uuid::new_v4();
        let connection = uuid::Uuid::new_v4();
        let descriptor =
            EndpointDescriptor::new(installation, std::env::current_exe().unwrap(), "test");
        let mut server = create_pipe(&descriptor.pipe_name, true).unwrap();
        let mut client = ClientOptions::new().open(&descriptor.pipe_name).unwrap();
        let task = tokio::spawn(async move {
            server.connect().await.unwrap();
            crate::IpcAuthenticator::new(installation)
                .accept(&mut server, |_| Ok(crate::Secret::from_bytes([3; 32])))
                .await
                .unwrap();
            let mut bytes = Vec::new();
            server.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        verify_server(&client, &std::env::current_exe().unwrap()).unwrap();
        crate::authenticate_client(
            &mut client,
            installation,
            connection,
            &crate::Secret::from_bytes([3; 32]),
        )
        .await
        .unwrap();
        client
            .write_all("{\"text\":\"français 💡\"}\n".as_bytes())
            .await
            .unwrap();
        drop(client);
        assert_eq!(
            task.await.unwrap(),
            "{\"text\":\"français 💡\"}\n".as_bytes()
        );
    }
}
