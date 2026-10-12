mod form;
mod main_view;
mod term_view;

use std::io::stdout;
use std::time::Duration;

use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind};
use form::{Form, FormResult};
use main_view::{MainView, Outcome};
use manageriliki_core::Manager;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::{DefaultTerminal, Frame};

enum Screen {
    Setup(Form),
    Login(Form),
    Main(Box<MainView>),
}

struct App {
    mgr: Manager,
    screen: Screen,
    quit: bool,
}

fn login_form() -> Form {
    Form::new("manageriliki — sign in")
        .submit_label("sign in")
        .text("username", "Username", "")
        .secret("password", "Password")
}

impl App {
    fn new(mgr: Manager) -> anyhow::Result<Self> {
        let screen = if mgr.needs_setup()? {
            Screen::Setup(
                Form::new("manageriliki — first-time setup")
                    .hint("Create the first administrator account.")
                    .submit_label("create")
                    .text("username", "Admin username", "admin")
                    .secret("password", "Password")
                    .secret("confirm", "Confirm password"),
            )
        } else {
            Screen::Login(login_form())
        };
        Ok(App { mgr, screen, quit: false })
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> anyhow::Result<()> {
        while !self.quit {
            if let Screen::Main(main) = &mut self.screen {
                main.tick();
            }
            terminal.draw(|f| self.draw(f))?;
            if !event::poll(Duration::from_millis(30))? {
                continue;
            }
            // Drain everything that is queued so pastes and fast typing stay responsive.
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => self.on_key(k),
                    Event::Paste(text) => {
                        if let Screen::Main(main) = &mut self.screen {
                            main.on_paste(&text);
                        }
                    }
                    _ => {}
                }
                if self.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        Ok(())
    }

    fn on_key(&mut self, key: crossterm::event::KeyEvent) {
        let next = match &mut self.screen {
            Screen::Setup(form) => match form.handle_key(key) {
                FormResult::Cancel => {
                    self.quit = true;
                    None
                }
                FormResult::Pending => None,
                FormResult::Submit => {
                    if form.get("password") != form.get("confirm") {
                        form.error = Some("passwords do not match".into());
                        None
                    } else {
                        match self.mgr.create_initial_admin(&form.get("username"), &form.get("password")) {
                            Ok(who) => Some(Screen::Main(Box::new(MainView::new(self.mgr.clone(), who)))),
                            Err(e) => {
                                form.error = Some(e.to_string());
                                None
                            }
                        }
                    }
                }
            },
            Screen::Login(form) => match form.handle_key(key) {
                FormResult::Cancel => {
                    self.quit = true;
                    None
                }
                FormResult::Pending => None,
                FormResult::Submit => match self.mgr.login(&form.get("username"), &form.get("password")) {
                    Ok(who) => Some(Screen::Main(Box::new(MainView::new(self.mgr.clone(), who)))),
                    Err(e) => {
                        form.error = Some(e.to_string());
                        None
                    }
                },
            },
            Screen::Main(main) => match main.on_key(key) {
                Outcome::Stay => None,
                Outcome::Quit => {
                    self.quit = true;
                    None
                }
                Outcome::Logout => Some(Screen::Login(login_form())),
            },
        };
        if let Some(screen) = next {
            self.screen = screen;
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        match &mut self.screen {
            Screen::Setup(form) | Screen::Login(form) => {
                let [area, footer] =
                    Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
                form.render(frame, area);
                frame.render_widget(
                    Line::from(format!(" data: {}", self.mgr.data_dir().display())).dim(),
                    footer,
                );
            }
            Screen::Main(main) => main.draw(frame),
        }
    }
}

fn main() -> anyhow::Result<()> {
    let mgr = Manager::open_default()?;
    let app = App::new(mgr)?;
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(stdout(), EnableBracketedPaste);
    let result = app.run(&mut terminal);
    let _ = crossterm::execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    result
}
