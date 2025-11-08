use std::io::Read;

#[test]
fn test_pty_execution_detects_tty() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    // This test verifies that commands executed through PTY
    // correctly detect that they are running in a terminal

    let pty_system = native_pty_system();
    let pty_size = PtySize {
        rows: 24,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty_pair = pty_system.openpty(pty_size)
        .expect("Failed to open PTY");

    // Use a command that checks if stdout is a TTY
    // The `test -t 1` command returns 0 (success) if stdout is a TTY
    let mut cmd = CommandBuilder::new("sh");
    cmd.args(vec!["-c", "test -t 1 && echo 'IS_TTY' || echo 'NOT_TTY'"]);
    cmd.env("TERM", "xterm-256color");

    let mut child = pty_pair.slave.spawn_command(cmd)
        .expect("Failed to spawn command");

    let mut reader = pty_pair.master.try_clone_reader()
        .expect("Failed to clone reader");

    let reader_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut read_buf = [0u8; 8192];

        loop {
            match reader.read(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => buffer.extend_from_slice(&read_buf[..n]),
                Err(_) => break,
            }
        }
        buffer
    });

    let exit_status = child.wait().expect("Failed to wait");
    let output = reader_thread.join().expect("Thread panicked");
    let output_str = String::from_utf8_lossy(&output);

    // Verify the command succeeded
    assert_eq!(exit_status.exit_code(), 0, "Command should exit successfully");

    // Verify that the output indicates we're running in a TTY
    assert!(
        output_str.contains("IS_TTY"),
        "Command should detect TTY when running through PTY. Output: {}",
        output_str
    );
}

#[test]
fn test_pty_sets_term_variable() {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    // Verify that TERM environment variable is properly set

    let pty_system = native_pty_system();
    let pty_size = PtySize {
        rows: 24,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    };

    let pty_pair = pty_system.openpty(pty_size)
        .expect("Failed to open PTY");

    let mut cmd = CommandBuilder::new("sh");
    cmd.args(vec!["-c", "echo $TERM"]);
    cmd.env("TERM", "xterm-256color");

    let mut child = pty_pair.slave.spawn_command(cmd)
        .expect("Failed to spawn command");

    let mut reader = pty_pair.master.try_clone_reader()
        .expect("Failed to clone reader");

    let reader_thread = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let mut read_buf = [0u8; 8192];

        loop {
            match reader.read(&mut read_buf) {
                Ok(0) => break,
                Ok(n) => buffer.extend_from_slice(&read_buf[..n]),
                Err(_) => break,
            }
        }
        buffer
    });

    let _exit_status = child.wait().expect("Failed to wait");
    let output = reader_thread.join().expect("Thread panicked");
    let output_str = String::from_utf8_lossy(&output);

    // Verify TERM is set correctly
    assert!(
        output_str.contains("xterm-256color"),
        "TERM variable should be set to xterm-256color. Output: {}",
        output_str
    );
}
