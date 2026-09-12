//! Bounded stdin forwarding that keeps owner disconnection observable under backpressure.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd};
use std::process::ChildStdin;

pub(super) fn forward_to(mut child: ChildStdin) -> io::Result<()> {
    let mut parent = File::from(io::stdin().as_fd().try_clone_to_owned()?);
    forward(&mut parent, &mut child)
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: pipe retains a live descriptor throughout both calls. F_GETFL and
    // F_SETFL take integer flags; this thread owns the child pipe's only writer.
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the same live descriptor and existing flags are preserved.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn forward(
    parent: &mut (impl Read + AsRawFd),
    child: &mut (impl Write + AsRawFd),
) -> io::Result<()> {
    nonblocking(child)?;
    let mut buffer = [0; 4096];
    let mut pending = 0..0;
    loop {
        let mut descriptors = [
            libc::pollfd {
                fd: parent.as_raw_fd(),
                events: if pending.is_empty() { libc::POLLIN } else { 0 },
                revents: 0,
            },
            libc::pollfd {
                fd: if pending.is_empty() {
                    -1
                } else {
                    child.as_raw_fd()
                },
                events: libc::POLLOUT,
                revents: 0,
            },
        ];
        // SAFETY: both descriptors stay owned by the function's borrowed inputs;
        // descriptors points to two initialized, writable pollfd records.
        if unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                -1,
            )
        } < 0
        {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        // The parent's pipe is a lifetime signal. EOF cancels the child even
        // when queued data remains and its stdin is full.
        if descriptors[0].revents & libc::POLLHUP != 0 {
            return Ok(());
        }
        if descriptors[0].revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
            return Err(io::Error::other("process owner input is unavailable"));
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            match parent.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(count) => pending = 0..count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        if descriptors[1].revents & libc::POLLNVAL != 0 {
            return Err(io::Error::other("owned process input is unavailable"));
        }
        if descriptors[1].revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "owned process closed its input",
            ));
        }
        if descriptors[1].revents & libc::POLLOUT != 0 {
            match child.write(&buffer[pending.clone()]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(count) => pending.start += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::FromRawFd;
    use std::sync::mpsc;
    use std::time::Duration;

    fn pipe() -> (File, File) {
        let mut descriptors = [0; 2];
        // SAFETY: descriptors provides writable storage for both newly owned FDs.
        assert_eq!(
            unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) },
            0,
            "create isolated forwarding pipes"
        );
        // SAFETY: successful pipe2 returned two distinct FDs, transferred exactly once.
        unsafe {
            (
                File::from_raw_fd(descriptors[0]),
                File::from_raw_fd(descriptors[1]),
            )
        }
    }

    #[test]
    fn owner_eof_cancels_forwarding_even_when_the_child_pipe_is_full() {
        let (mut parent, mut control) = pipe();
        let (application, mut child) = pipe();
        nonblocking(&child).expect("make the child pipe fill observable");
        loop {
            match child.write(&[0; 4096]) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill child input: {error}"),
            }
        }
        control
            .write_all(b"queued configuration")
            .expect("queue parent data behind the full child pipe");
        drop(control);
        let (sender, receiver) = mpsc::channel();
        let thread =
            std::thread::spawn(move || sender.send(forward(&mut parent, &mut child)).unwrap());
        let result = receiver.recv_timeout(Duration::from_secs(5));
        drop(application);
        thread.join().expect("forwarding worker must stop");
        assert!(
            matches!(result, Ok(Ok(()))),
            "owner EOF must remain observable under child backpressure: {result:?}"
        );
    }

    #[test]
    fn configuration_bytes_reach_the_child_while_its_owner_remains_connected() {
        let (mut parent, mut control) = pipe();
        let (mut application, mut child) = pipe();
        let forwarder = std::thread::spawn(move || forward(&mut parent, &mut child));
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4];
            let result = application.read_exact(&mut bytes).map(|()| bytes);
            sender.send(result).unwrap();
        });
        control
            .write_all(b"test")
            .expect("send configuration bytes");
        let received = receiver.recv_timeout(Duration::from_secs(5));
        drop(control);
        reader.join().expect("application reader must stop");
        assert!(
            matches!(received, Ok(Ok(bytes)) if bytes == *b"test"),
            "forwarding must preserve the parent's bytes: {received:?}"
        );
        assert!(
            forwarder.join().expect("join forwarding worker").is_ok(),
            "closing the control pipe must finish forwarding"
        );
    }
}
