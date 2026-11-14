extern "env" fn oni_write_pty(terminal: u32, userdata: u32, data: u32, len: u32) void;
extern "env" fn oni_color_scheme(terminal: u32, userdata: u32, out_scheme: u32) bool;

export fn ghostty_write_pty(terminal: u32, userdata: u32, data: u32, len: u32) void {
    oni_write_pty(terminal, userdata, data, len);
}

// `out_scheme` points into libghostty-vt's memory, not this module's, so JS writes it.
export fn ghostty_color_scheme(terminal: u32, userdata: u32, out_scheme: u32) bool {
    return oni_color_scheme(terminal, userdata, out_scheme);
}
