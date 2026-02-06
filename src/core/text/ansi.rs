//! ANSI parsing and style tracking (Phase 3).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiCodeKind {
    Csi,
    Osc,
    Apc,
    Dcs,
    Ss3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiCode {
    pub code: String,
    pub length: usize,
    pub kind: AnsiCodeKind,
}

pub fn extract_ansi_code(input: &str, pos: usize) -> Option<AnsiCode> {
    let bytes = input.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return None;
    }
    if pos + 1 >= bytes.len() {
        return None;
    }

    match bytes[pos + 1] {
        b'[' => extract_csi(input, pos),
        b']' => extract_osc(input, pos),
        b'_' => extract_apc(input, pos),
        b'P' => extract_dcs(input, pos),
        b'O' => extract_ss3(input, pos),
        _ => None,
    }
}

fn extract_csi(input: &str, pos: usize) -> Option<AnsiCode> {
    let bytes = input.as_bytes();
    let mut idx = pos + 2;
    while idx < bytes.len() {
        let b = bytes[idx];
        if (0x40..=0x7e).contains(&b) {
            let end = idx + 1;
            return Some(AnsiCode {
                code: input[pos..end].to_string(),
                length: end - pos,
                kind: AnsiCodeKind::Csi,
            });
        }
        idx += 1;
    }
    None
}

fn extract_osc(input: &str, pos: usize) -> Option<AnsiCode> {
    extract_string_terminated(input, pos, AnsiCodeKind::Osc)
}

fn extract_apc(input: &str, pos: usize) -> Option<AnsiCode> {
    extract_string_terminated(input, pos, AnsiCodeKind::Apc)
}

fn extract_dcs(input: &str, pos: usize) -> Option<AnsiCode> {
    extract_string_terminated(input, pos, AnsiCodeKind::Dcs)
}

fn extract_ss3(input: &str, pos: usize) -> Option<AnsiCode> {
    let bytes = input.as_bytes();
    if pos + 2 >= bytes.len() {
        return None;
    }
    let end = pos + 3;
    Some(AnsiCode {
        code: input[pos..end].to_string(),
        length: end - pos,
        kind: AnsiCodeKind::Ss3,
    })
}

fn extract_string_terminated(input: &str, pos: usize, kind: AnsiCodeKind) -> Option<AnsiCode> {
    let bytes = input.as_bytes();
    let mut idx = pos + 2;
    while idx < bytes.len() {
        if bytes[idx] == 0x07 {
            let end = idx + 1;
            return Some(AnsiCode {
                code: input[pos..end].to_string(),
                length: end - pos,
                kind,
            });
        }
        if bytes[idx] == 0x1b && idx + 1 < bytes.len() && bytes[idx + 1] == b'\\' {
            let end = idx + 2;
            return Some(AnsiCode {
                code: input[pos..end].to_string(),
                length: end - pos,
                kind,
            });
        }
        idx += 1;
    }
    None
}

#[derive(Debug, Default)]
pub struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
}

impl AnsiCodeTracker {
    pub fn process(&mut self, ansi_code: &str) {
        if !ansi_code.ends_with('m') {
            return;
        }

        let Some(params) = ansi_code.strip_prefix("\x1b[") else {
            return;
        };
        let Some(params) = params.strip_suffix('m') else {
            return;
        };

        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }

        let parts: Vec<&str> = params.split(';').collect();
        let mut idx = 0;
        while idx < parts.len() {
            let code = parts[idx].parse::<u16>().unwrap_or(0);
            if code == 38 || code == 48 {
                if idx + 2 < parts.len() && parts[idx + 1] == "5" {
                    let color_code =
                        format!("{};{};{}", parts[idx], parts[idx + 1], parts[idx + 2]);
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    idx += 3;
                    continue;
                }
                if idx + 4 < parts.len() && parts[idx + 1] == "2" {
                    let color_code = format!(
                        "{};{};{};{};{}",
                        parts[idx],
                        parts[idx + 1],
                        parts[idx + 2],
                        parts[idx + 3],
                        parts[idx + 4]
                    );
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    idx += 5;
                    continue;
                }
            }

            match code {
                0 => self.reset(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.bold = false,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                39 => self.fg_color = None,
                49 => self.bg_color = None,
                30..=37 | 90..=97 => self.fg_color = Some(code.to_string()),
                40..=47 | 100..=107 => self.bg_color = Some(code.to_string()),
                _ => {}
            }
            idx += 1;
        }
    }

    pub fn clear(&mut self) {
        self.reset();
    }

    pub fn active_codes(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.blink {
            codes.push("5".to_string());
        }
        if self.inverse {
            codes.push("7".to_string());
        }
        if self.hidden {
            codes.push("8".to_string());
        }
        if self.strikethrough {
            codes.push("9".to_string());
        }
        if let Some(color) = self.fg_color.as_ref() {
            codes.push(color.clone());
        }
        if let Some(color) = self.bg_color.as_ref() {
            codes.push(color.clone());
        }

        if codes.is_empty() {
            return String::new();
        }

        format!("\x1b[{}m", codes.join(";"))
    }

    pub fn line_end_reset(&self) -> String {
        if self.underline {
            return "\x1b[24m".to_string();
        }
        String::new()
    }

    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
    }
}
