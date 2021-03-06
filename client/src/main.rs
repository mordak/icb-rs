#[allow(dead_code)]
mod util;
use util::{Event, Events};

#[macro_use]
extern crate clap;
use chrono::{Local, Timelike};
use clap::App;
use crossbeam_utils::thread;
use icb::{packets, Command, Config};
use std::io::{self, Write};
use std::process::exit;
use std::time::Duration;
use termion::clear;
use termion::cursor::Goto;
use termion::event::Key;
use termion::input::MouseTerminal;
use termion::raw::IntoRawMode;
use termion::screen::AlternateScreen;
use tui::backend::TermionBackend;
use tui::layout::{Constraint, Direction, Layout};
use tui::widgets::{Block, Borders, List, Paragraph, Text, Widget};
use tui::Terminal;
use unicode_width::UnicodeWidthStr;

struct Ui {
    input: String,
    history: Vec<String>,
    user_history: Vec<String>,
}

impl Default for Ui {
    fn default() -> Ui {
        Ui {
            input: String::new(),
            // History as shown (includes everything that happened in the group)
            history: Vec::new(),
            // What the user has sent so far
            user_history: Vec::with_capacity(100),
        }
    }
}

/// Create a timestamp for 'now', returned as 'HH:MM'.
fn timestamp() -> String {
    let now = Local::now();
    format!("{:02}:{:02}", now.hour(), now.minute())
}

fn main() -> Result<(), failure::Error> {
    let clap_yaml = load_yaml!("clap.yml");
    let matches = App::from_yaml(clap_yaml).get_matches();

    let nickname = matches.value_of("nickname").unwrap().to_string();
    let serverip = matches.value_of("hostname").unwrap().to_string();
    let port = value_t!(matches, "port", u16).unwrap_or(7326);
    let group = matches.value_of("group").unwrap().to_string();

    let config = Config {
        nickname,
        serverip,
        port,
        group: group.clone(),
    };

    let (mut client, mut server) = icb::init(config).unwrap();

    // Configure the terminal...
    let stdout = io::stdout().into_raw_mode()?;
    let stdout = MouseTerminal::from(stdout);
    let stdout = AlternateScreen::from(stdout);
    let backend = TermionBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // ...and event handlers...
    let events = Events::new();

    // ...and finally create the default UI state
    let mut ui = Ui::default();

    println!("{}", clear::All);

    let mut hist_offset = 0; // offset in the user_history vector (counted from the end)
    let mut cursor_offset = 0; // offset of the cursor from the end of the string

    thread::scope(|s| {
        s.spawn(|_| {
            server.run();
        });

        loop {
            // Handle any communication with the backend before drawing the next screen.
            if let Ok(m) = client.msg_r.try_recv() {
                let packet_type = m[0].chars().next().unwrap();
                match packet_type {
                    packets::T_OPEN => {
                        ui.history
                            .push(format!("{} <{}> {}", timestamp(), m[1], m[2]))
                    }
                    packets::T_PERSONAL => {
                        ui.history
                            .push(format!("{} **{}** {}", timestamp(), m[1], m[2]))
                    }
                    packets::T_PROTOCOL => ui
                        .history
                        .push(format!("==> Connected to {} on {}", m[2], m[1])),
                    packets::T_STATUS => match m[1].as_str() {
                        "Arrive" | "Boot" | "Depart" | "Help" | "Name" | "No-Beep" | "Notify"
                        | "Sign-off" | "Sign-on" | "Status" | "Topic" | "Warning" => {
                            ui.history.push(format!("{}: {} ", timestamp(), m[2]))
                        }
                        _ => ui.history.push(format!(
                            "=> Message '{}' received in unknown category '{}'",
                            m[2], m[1]
                        )),
                    },
                    packets::T_BEEP => {
                        ui.history
                            .push(format!("{} *{} beeps you*", timestamp(), m[1]))
                    }
                    // XXX: should handle "\x18eNick is already in use\x00" too
                    _ => ui
                        .history
                        .push(format!("msg_r: {} read: {:?}", timestamp(), m)),
                }
            }
            std::thread::sleep(Duration::from_millis(1));

            let termsize = terminal.size().unwrap();

            terminal
                .draw(|mut f| {
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .horizontal_margin(1)
                        .constraints(
                            [
                                Constraint::Length(1),
                                Constraint::Min(1),
                                Constraint::Length(3),
                            ]
                            .as_ref(),
                        )
                        .split(f.size());

                    // XXX: Keep track of the current group and topic
                    let help_message = format!("Group: {}", group);
                    Paragraph::new([Text::raw(help_message)].iter()).render(&mut f, chunks[0]);
                    Paragraph::new([Text::raw(&ui.input)].iter())
                        .block(Block::default().borders(Borders::TOP))
                        .render(&mut f, chunks[2]);
                    // XXX: using pageup/pagedown should allow for scrolling through
                    //      the history too.
                    let max_history_len = termsize.height
                        - 1  // chunks[0],
                        - 2  // chunk[1] + border
                        - 2; // chunks[2] + border

                    // Get the full history and take as many entries from the end as we can fit
                    // in the history pane of the window.
                    let history = ui
                        .history
                        .iter()
                        .rev()
                        .take(max_history_len as usize)
                        .rev()
                        .map(|i| Text::raw(i.to_string()));
                    List::new(history)
                        .block(Block::default().borders(Borders::TOP))
                        .render(&mut f, chunks[1]);
                })
                .expect("Failed to draw UI to terminal");

            // Put the cursor back inside the input box
            write!(
                terminal.backend_mut(),
                "{}",
                Goto(
                    2 + (ui.input.width() - cursor_offset) as u16,
                    termsize.height - 1
                )
            )
            .expect("Failed to position cursor");
            io::stdout().flush().ok();

            // Now read the user input, these could be control actions such as backspace,
            // commands (starting with '/') or actual messages intended for other users.
            if let Event::Input(input) = events.next().expect("Failed to read user input") {
                match input {
                    Key::Backspace => {
                        ui.input.pop();
                    }
                    Key::Ctrl(c) => match c {
                        'w' => {
                            // XXX: should only remove the last word instead of the whole line
                            ui.input.clear();
                            cursor_offset = 0;
                        }
                        // Move the cursor to the beginning of the line
                        'a' => cursor_offset = ui.input.width(),
                        // Move the cursor to the end of the line
                        'e' => cursor_offset = 0,
                        _ => {}
                    },
                    Key::Up => {
                        // Replace the current line with the entry in the user_history vector at
                        // the offset hist_offset, counted from the end.
                        hist_offset += 1;
                        cursor_offset = 0;

                        let hist_len = ui.user_history.len();

                        if (hist_offset > hist_len) || (hist_len == 0) {
                            continue;
                        }

                        ui.input = ui.user_history[hist_len - hist_offset].to_string();
                    }
                    Key::Down => {
                        // Do the same as for the Up key, but in reverse. Take care not to
                        // make the offset negative. If the offset would become zero, just
                        // clear the input field.
                        cursor_offset = 0;
                        let hist_len = ui.user_history.len();
                        if hist_offset == 0 || hist_len == 0 {
                            continue;
                        } else if hist_offset == 1 {
                            ui.input.drain(..);
                            continue;
                        }

                        hist_offset -= 1;

                        ui.input = ui.user_history[hist_len - hist_offset].to_string();
                    }
                    Key::Left => {
                        if cursor_offset == ui.input.width() {
                            continue;
                        }

                        cursor_offset += 1;
                    }
                    Key::Right => {
                        if cursor_offset == 0 {
                            continue;
                        }

                        cursor_offset -= 1;
                    }
                    Key::Char('\n') => {
                        // Reset the position in the user_history buffer as well as the offset into
                        // the input field for the cursor.
                        hist_offset = 0;
                        cursor_offset = 0;

                        match ui.input.chars().next() {
                            Some(v) if v == '/' => {
                                let input: Vec<_> = ui.input.split_whitespace().collect();
                                let cmd = input[0];

                                if cmd == "/quit" {
                                    // Use a hammer to quit, ICB doesn't provide a clean way to
                                    // disconnect anyway other than terminating the conneciton.
                                    io::stdout().flush().ok();
                                    exit(0);
                                } else if (cmd == "/msg" || cmd == "/m") && input.len() > 2 {
                                    let recipient = input[1];

                                    // Now take the text the user has entered and remove the first
                                    // occurences of the command and recipient. We explicitly don't
                                    // use `input` as we may lose any duplicate whitespace the sender has
                                    // inserted, but remove the space after the recipient name.
                                    let msg_text: String = ui.input.replacen(cmd, "", 1).replacen(
                                        format!(" {} ", recipient).as_str(),
                                        "",
                                        1,
                                    );
                                    let msg = Command::Personal(
                                        recipient.to_string().clone(),
                                        msg_text.clone(),
                                    );
                                    client.cmd_s.send(msg).unwrap();

                                    // Record the normalized command
                                    ui.user_history
                                        .push(format!("{} {} {}", cmd, recipient, msg_text));
                                    ui.history.push(format!(
                                        "{}: -> {}: {}",
                                        timestamp(),
                                        recipient,
                                        msg_text
                                    ));

                                    ui.input.drain(..);
                                } else if cmd == "/beep" && input.len() == 2 {
                                    let recipient = input[1];

                                    let msg = Command::Beep(recipient.to_string());
                                    client.cmd_s.send(msg).unwrap();

                                    ui.user_history.push(format!("{} {}", cmd, recipient));
                                    ui.history.push(format!(
                                        "{}: *beep beep, {}*",
                                        timestamp(),
                                        recipient
                                    ));
                                    ui.input.drain(..);
                                } else if (cmd == "/name" || cmd == "/nick") && input.len() == 2 {
                                    let newname = input[1];

                                    let msg = Command::Name(newname.to_string());
                                    client.cmd_s.send(msg).unwrap();
                                    ui.user_history.push(format!("{} {}", cmd, newname));
                                    client.nickname = newname.to_string();
                                    ui.input.drain(..);
                                }
                            }
                            _ => {
                                let msg_text: String = ui.input.drain(..).collect();

                                let msg = Command::Open(msg_text.clone());
                                client.cmd_s.send(msg).unwrap();

                                // Send our own messages into the history as well as the server
                                // won't echo them back to us.
                                ui.history.push(format!("{}: {}", timestamp(), msg_text));
                                ui.user_history.push(msg_text);
                                ui.input.clear();
                            }
                        }
                    }
                    Key::Char(c) => {
                        // Determine where new characters should end up; simple case is just
                        // at the end.
                        if cursor_offset == 0 {
                            ui.input.push(c);
                        } else {
                            // Otherwise have to insert the new character into ui.input at the provided
                            // (negative) offset.
                            let input_len = ui.input.len();
                            ui.input.insert(input_len - cursor_offset, c);
                        }
                    }
                    _ => {}
                }
            }
        }
    })
    .unwrap();

    Ok(())
}
