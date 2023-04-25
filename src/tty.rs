use log::debug;
use std::sync::atomic::{AtomicBool, Ordering};

pub static UPDATE_WINSIZE: AtomicBool = AtomicBool::new(false);

pub struct Tty {
    saved_termios: libc::termios,
}

impl Tty {
    pub fn new() -> Self {
        unsafe {
            debug!("Set up TTY");
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = Self::signal_handler as libc::sighandler_t;
            libc::sigaction(libc::SIGWINCH, &act, std::ptr::null_mut());
            let mut saved_termios: libc::termios = std::mem::zeroed();
            libc::tcgetattr(libc::STDIN_FILENO, &mut saved_termios);
            let mut revsh_termios = saved_termios.clone();

            libc::cfmakeraw(&mut revsh_termios);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &revsh_termios);

            Self { saved_termios }
        }
    }
    pub fn get_winsize() -> libc::winsize {
        unsafe {
            let winsize: libc::winsize = std::mem::zeroed();
            libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &winsize);
            winsize
        }
    }
    pub fn get_term_size() -> (u16, u16) {
        let winsize = Tty::get_winsize();
        let term_width = winsize.ws_row;
        let term_height = winsize.ws_col;
        (term_width, term_height)
    }

    fn signal_handler(signal: i32) {
        if signal == libc::SIGWINCH {
            UPDATE_WINSIZE.store(true, Ordering::Relaxed);
        }
    }
}

impl Drop for Tty {
    fn drop(&mut self) {
        unsafe {
            debug!("Leaving of TTY");
            libc::signal(libc::SIGWINCH, libc::SIG_DFL);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved_termios);
        }
    }
}

impl Default for Tty {
    fn default() -> Self {
        Self::new()
    }
}
