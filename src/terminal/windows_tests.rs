use super::*;
use std::{
    fs::OpenOptions,
    os::windows::{io::AsRawHandle, process::CommandExt},
    process::Command,
};
use windows_sys::Win32::System::{
    Console::{
        COORD, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_INPUT,
        ENABLE_QUICK_EDIT_MODE, GetConsoleMode, GetStdHandle, ReadConsoleOutputCharacterW,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle, WriteConsoleOutputCharacterW,
    },
    Threading::CREATE_NEW_CONSOLE,
};

// Each scenario needs a fresh process: Crossterm caches the first mouse mode.
// An isolated console also keeps CI's redirected streams and other tests untouched.
#[test]
fn console_lifecycle() {
    const SCENARIO: &str = "DANMU_TEST_CONSOLE_SCENARIO";
    let Ok(scenario) = std::env::var(SCENARIO) else {
        for scenario in ["no_capture", "toggle_capture", "exit_with_capture"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "terminal::windows_tests::console_lifecycle",
                    "--nocapture",
                ])
                .env(SCENARIO, scenario)
                .creation_flags(CREATE_NEW_CONSOLE)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{scenario}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        return;
    };

    // Rust's test runner captures stdio; bind the child to its actual console.
    // Keep stderr redirected so assertion failures reach the parent/CI log.
    let input = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONIN$")
        .unwrap();
    let output = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .unwrap();
    // SAFETY: Querying process standard handles does not transfer ownership.
    let original_stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    let original_stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    let marker: Vec<u16> = "original shell screen".encode_utf16().collect();
    let mut written = 0;
    // SAFETY: The output handle, UTF-16 buffer, and count pointer are valid.
    assert_ne!(
        unsafe {
            WriteConsoleOutputCharacterW(
                output.as_raw_handle(),
                marker.as_ptr(),
                marker.len() as u32,
                COORD { X: 0, Y: 0 },
                &mut written,
            )
        },
        0
    );
    assert_eq!(written as usize, marker.len());
    // SAFETY: Both handles stay open until all terminal operations finish.
    unsafe {
        assert_ne!(SetStdHandle(STD_INPUT_HANDLE, input.as_raw_handle()), 0);
        assert_ne!(SetStdHandle(STD_OUTPUT_HANDLE, output.as_raw_handle()), 0);
    }
    let input_mode = || {
        let mut mode = 0;
        // SAFETY: The live input handle and writable mode pointer are valid.
        assert_ne!(
            unsafe { GetConsoleMode(input.as_raw_handle(), &mut mode) },
            0
        );
        mode
    };
    let cooked = ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT;
    let original = input_mode();
    assert_eq!(original & cooked, cooked, "fresh console must be cooked");

    let mut terminal = TerminalGuard::enter().expect("cold Windows TUI startup");
    let raw = input_mode();
    assert_eq!(
        raw & cooked,
        0,
        "TUI must receive keys without line buffering"
    );
    terminal.set_mouse_capture(false).unwrap();
    match scenario.as_str() {
        "no_capture" => {}
        "toggle_capture" | "exit_with_capture" => {
            terminal.set_mouse_capture(true).unwrap();
            assert_ne!(input_mode() & ENABLE_MOUSE_INPUT, 0);
            assert_eq!(input_mode() & ENABLE_QUICK_EDIT_MODE, 0);
            assert_eq!(input_mode() & cooked, 0);
            if scenario == "toggle_capture" {
                terminal.set_mouse_capture(false).unwrap();
                assert_eq!(input_mode(), raw, "closing a popup must preserve raw input");
                terminal.set_mouse_capture(true).unwrap();
                terminal.set_mouse_capture(false).unwrap();
                assert_eq!(
                    input_mode(),
                    raw,
                    "reopening a popup must not change the baseline"
                );
            }
        }
        _ => panic!("unknown console scenario: {scenario}"),
    }
    // Exercise the real backend, not just command construction.
    terminal
        .terminal
        .draw(|frame| frame.render_widget(Paragraph::new("console lifecycle"), frame.area()))
        .unwrap();
    drop(terminal);
    assert_eq!(
        input_mode(),
        original,
        "exit must restore shell input and mouse modes"
    );
    let active_output = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")
        .unwrap();
    let mut restored = vec![0; marker.len()];
    let mut read = 0;
    // SAFETY: The active console handle and writable buffers are valid.
    assert_ne!(
        unsafe {
            ReadConsoleOutputCharacterW(
                active_output.as_raw_handle(),
                restored.as_mut_ptr(),
                restored.len() as u32,
                COORD { X: 0, Y: 0 },
                &mut read,
            )
        },
        0
    );
    assert_eq!(read as usize, marker.len());
    assert_eq!(
        restored, marker,
        "exit must return to the original shell screen"
    );
    // SAFETY: The inherited handles remain owned by the child/test runner.
    unsafe {
        assert_ne!(SetStdHandle(STD_INPUT_HANDLE, original_stdin), 0);
        assert_ne!(SetStdHandle(STD_OUTPUT_HANDLE, original_stdout), 0);
    }
}
