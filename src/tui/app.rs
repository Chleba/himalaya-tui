use color_eyre::Result;
use crate::config::TomlConfig;
use ratatui::{prelude::*, widgets::*};
use std::time::{Duration, Instant};
use crossterm::event::{self, Event, KeyCode};
use pimalaya_tui::{
    himalaya::backend::BackendBuilder,
    terminal::config::TomlConfig as _,
};
use std::sync::Arc;
use email::backend::feature::BackendFeatureSource;
use email::envelope::list::ListEnvelopesOptions;
use email::folder::list::ListFolders;
use email::config::Config;
use pimalaya_tui::himalaya::config::{Envelopes, Folders};
use crate::cli::Cli;
use pimalaya_tui::terminal::cli::printer::StdoutPrinter;
use crossterm::{
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
};

#[derive(Copy, Clone)]
pub enum Focus {
    Folders,
    Envelopes,
    Message,
}

pub enum Action {
    None,
    Write,
    Reply(String),
    Forward(String),
    Download(String),
}

pub struct App {
    pub config: TomlConfig,
    pub envelopes: Envelopes,
    pub envelopes_state: TableState,
    pub folders: Folders,
    pub folders_state: ListState,
    pub current_folder: String,
    pub current_page: usize,
    pub current_message: Option<String>,
    pub current_message_subject: Option<String>,
    pub message_scroll: u16,
    pub focus: Focus,
    pub show_help: bool,
    pub last_tick: Instant,
    pub tick_rate: Duration,
    pub should_quit: bool,
}

impl App {
    pub async fn new(config: TomlConfig) -> Result<Self> {
        let (folders, envelopes) = Self::initial_fetch(&config).await?;
        
        let mut folders_state = ListState::default();
        folders_state.select(Some(0));

        let mut envelopes_state = TableState::default();
        if !envelopes.is_empty() {
            envelopes_state.select(Some(0));
        }

        Ok(Self {
            config,
            envelopes,
            envelopes_state,
            folders,
            folders_state,
            current_folder: "INBOX".to_string(),
            current_page: 0,
            current_message: None,
            current_message_subject: None,
            message_scroll: 0,
            focus: Focus::Envelopes,
            show_help: false,
            last_tick: Instant::now(),
            tick_rate: Duration::from_secs(60),
            should_quit: false,
        })
    }

    async fn initial_fetch(config: &TomlConfig) -> Result<(Folders, Envelopes)> {
        let (toml_account_config, account_config) = config
            .clone()
            .into_account_configs(None, |c: &Config, name| c.account(name).ok())?;
        let toml_account_config = Arc::new(toml_account_config);
        let account_config = Arc::new(account_config);

        let backend = BackendBuilder::new(
            toml_account_config.clone(),
            account_config.clone(),
            |builder| {
                builder
                    .without_features()
                    .with_list_folders(BackendFeatureSource::Context)
                    .with_list_envelopes(BackendFeatureSource::Context)
            },
        )
        .without_sending_backend()
        .build()
        .await?;
        
        let folders = Folders::from(backend.list_folders().await?);
        
        let opts = ListEnvelopesOptions {
            page: 0,
            page_size: account_config.get_envelope_list_page_size(),
            query: None,
        };

        let envelopes = backend.list_envelopes("INBOX", opts).await?;
        
        Ok((folders, envelopes))
    }

    async fn fetch_folders(config: &TomlConfig) -> Result<Folders> {
        let (toml_account_config, account_config) = config
            .clone()
            .into_account_configs(None, |c: &Config, name| c.account(name).ok())?;
        let toml_account_config = Arc::new(toml_account_config);
        let account_config = Arc::new(account_config);

        let backend = BackendBuilder::new(
            toml_account_config.clone(),
            account_config.clone(),
            |builder| {
                builder
                    .without_features()
                    .with_list_folders(BackendFeatureSource::Context)
            },
        )
        .without_sending_backend()
        .build()
        .await?;
        
        Ok(Folders::from(backend.list_folders().await?))
    }

    async fn fetch_envelopes(config: &TomlConfig, folder: &str, page: usize) -> Result<Envelopes> {
        let (toml_account_config, account_config) = config
            .clone()
            .into_account_configs(None, |c: &Config, name| c.account(name).ok())?;
        let toml_account_config = Arc::new(toml_account_config);
        let account_config = Arc::new(account_config);

        let backend = BackendBuilder::new(
            toml_account_config.clone(),
            account_config.clone(),
            |builder| {
                builder
                    .without_features()
                    .with_list_envelopes(BackendFeatureSource::Context)
            },
        )
        .without_sending_backend()
        .build()
        .await?;

        let opts = ListEnvelopesOptions {
            page,
            page_size: account_config.get_envelope_list_page_size(),
            query: None,
        };
        Ok(backend.list_envelopes(folder, opts).await?)
    }

    async fn fetch_message(config: &TomlConfig, folder: &str, id: usize) -> Result<String> {
        let (toml_account_config, account_config) = config
            .clone()
            .into_account_configs(None, |c: &Config, name| c.account(name).ok())?;
        let toml_account_config = Arc::new(toml_account_config);
        let account_config = Arc::new(account_config);

        let backend = BackendBuilder::new(
            toml_account_config.clone(),
            account_config.clone(),
            |builder| {
                builder
                    .without_features()
                    .with_peek_messages(BackendFeatureSource::Context)
            },
        )
        .without_sending_backend()
        .build()
        .await?;
        
        let emails = backend.peek_messages(folder, &[id]).await?;
        let mut bodies = String::default();
        let mut glue = "";
        
        for email in emails.to_vec() {
            bodies.push_str(glue);
            let tpl = email.to_read_tpl(&account_config, |tpl| tpl).await?;
            bodies.push_str(&tpl);
            glue = "\n\n";
        }
        
        Ok(bodies)
    }

    pub async fn run<B: ratatui::backend::Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            let timeout = self.tick_rate
                .checked_sub(self.last_tick.elapsed())
                .unwrap_or(Duration::from_secs(0));

            if event::poll(timeout)? {
                if let Event::Key(key) = event::read()? {
                    let action = self.handle_input(key).await?;
                    if matches!(action, Action::Write | Action::Reply(_) | Action::Forward(_) | Action::Download(_)) {
                        self.suspend_and_run(terminal, action).await?;
                        // Refresh after sending/saving
                        if let Ok(envelopes) = Self::fetch_envelopes(&self.config, &self.current_folder, self.current_page).await {
                            self.envelopes = envelopes;
                            self.envelopes_state.select(Some(0));
                        }
                        if let Ok(folders) = Self::fetch_folders(&self.config).await {
                            self.folders = folders;
                        }
                    }
                }
            }

            if self.last_tick.elapsed() >= self.tick_rate {
                if let Ok(envelopes) = Self::fetch_envelopes(&self.config, &self.current_folder, self.current_page).await {
                    self.envelopes = envelopes;
                }
                if let Ok(folders) = Self::fetch_folders(&self.config).await {
                    self.folders = folders;
                }
                self.last_tick = Instant::now();
            }

            if self.should_quit {
                break;
            }
        }
        Ok(())
    }

    async fn suspend_and_run<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        action: Action,
    ) -> Result<()> {
        disable_raw_mode()?;
        let mut stdout = std::io::stdout();
        execute!(
            stdout,
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;

        let mut printer = StdoutPrinter::default();
        let args = match &action {
            Action::Write => vec!["himalaya".to_string(), "message".to_string(), "write".to_string()],
            Action::Reply(id) => vec!["himalaya".to_string(), "message".to_string(), "reply".to_string(), id.clone()],
            Action::Forward(id) => vec!["himalaya".to_string(), "message".to_string(), "forward".to_string(), id.clone()],
            Action::Download(id) => vec!["himalaya".to_string(), "attachments".to_string(), "download".to_string(), id.clone()],
            Action::None => vec![],
        };

        if !args.is_empty() {
            use clap::Parser;
            if let Ok(cli) = Cli::try_parse_from(args) {
                if let Some(cmd) = cli.command {
                    let _ = cmd.execute(&mut printer, &[]).await;
                }
            }
        }

        enable_raw_mode()?;
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        terminal.clear()?;

        Ok(())
    }

    async fn handle_input(&mut self, key: event::KeyEvent) -> Result<Action> {
        if self.show_help {
            self.show_help = false;
            return Ok(Action::None);
        }

        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('R') => {
                if let Ok(envelopes) = Self::fetch_envelopes(&self.config, &self.current_folder, self.current_page).await {
                    self.envelopes = envelopes;
                    self.envelopes_state.select(Some(0));
                }
                if let Ok(folders) = Self::fetch_folders(&self.config).await {
                    self.folders = folders;
                }
            }
            KeyCode::Char('n') => {
                self.current_page += 1;
                if let Ok(envelopes) = Self::fetch_envelopes(&self.config, &self.current_folder, self.current_page).await {
                    self.envelopes = envelopes;
                    self.envelopes_state.select(Some(0));
                }
            }
            KeyCode::Char('p') => {
                if self.current_page > 0 {
                    self.current_page -= 1;
                    if let Ok(envelopes) = Self::fetch_envelopes(&self.config, &self.current_folder, self.current_page).await {
                        self.envelopes = envelopes;
                        self.envelopes_state.select(Some(0));
                    }
                }
            }
            KeyCode::Char('c') => return Ok(Action::Write),
            KeyCode::Char('r') => {
                if let Some(i) = self.envelopes_state.selected() {
                    if let Some(envelope) = self.envelopes.get(i) {
                        return Ok(Action::Reply(envelope.id.clone()));
                    }
                }
            }
            KeyCode::Char('f') => {
                if let Some(i) = self.envelopes_state.selected() {
                    if let Some(envelope) = self.envelopes.get(i) {
                        return Ok(Action::Forward(envelope.id.clone()));
                    }
                }
            }
            KeyCode::Char('d') => {
                if let Some(i) = self.envelopes_state.selected() {
                    if let Some(envelope) = self.envelopes.get(i) {
                        return Ok(Action::Download(envelope.id.clone()));
                    }
                }
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Folders => Focus::Envelopes,
                    Focus::Envelopes => Focus::Message,
                    Focus::Message => Focus::Folders,
                };
            }
            KeyCode::Char('1') => self.focus = Focus::Folders,
            KeyCode::Char('2') => self.focus = Focus::Envelopes,
            KeyCode::Char('3') => self.focus = Focus::Message,
            KeyCode::Down | KeyCode::Char('j') => {
                match self.focus {
                    Focus::Folders => {
                        let i = match self.folders_state.selected() {
                            Some(i) => if i >= self.folders.len().saturating_sub(1) { 0 } else { i + 1 },
                            None => 0,
                        };
                        self.folders_state.select(Some(i));
                    }
                    Focus::Envelopes => {
                        let i = match self.envelopes_state.selected() {
                            Some(i) => if i >= self.envelopes.len().saturating_sub(1) { 0 } else { i + 1 },
                            None => 0,
                        };
                        self.envelopes_state.select(Some(i));
                    }
                    Focus::Message => {
                        self.message_scroll = self.message_scroll.saturating_add(1);
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                match self.focus {
                    Focus::Folders => {
                        let i = match self.folders_state.selected() {
                            Some(i) => if i == 0 { self.folders.len().saturating_sub(1) } else { i - 1 },
                            None => 0,
                        };
                        self.folders_state.select(Some(i));
                    }
                    Focus::Envelopes => {
                        let i = match self.envelopes_state.selected() {
                            Some(i) => if i == 0 { self.envelopes.len().saturating_sub(1) } else { i - 1 },
                            None => 0,
                        };
                        self.envelopes_state.select(Some(i));
                    }
                    Focus::Message => {
                        self.message_scroll = self.message_scroll.saturating_sub(1);
                    }
                }
            }
            KeyCode::Enter => {
                match self.focus {
                    Focus::Folders => {
                        if let Some(i) = self.folders_state.selected() {
                            if let Some(folder) = self.folders.get(i) {
                                self.current_folder = folder.name.clone();
                                self.current_page = 0;
                                self.current_message = None;
                                self.current_message_subject = None;
                                if let Ok(envelopes) = Self::fetch_envelopes(&self.config, &self.current_folder, self.current_page).await {
                                    self.envelopes = envelopes;
                                    self.envelopes_state.select(Some(0));
                                }
                                self.focus = Focus::Envelopes;
                            }
                        }
                    }
                    Focus::Envelopes => {
                        if let Some(i) = self.envelopes_state.selected() {
                            if let Some(envelope) = self.envelopes.get(i) {
                                if let Ok(id) = envelope.id.parse::<usize>() {
                                    if let Ok(msg) = Self::fetch_message(&self.config, &self.current_folder, id).await {
                                        self.current_message = Some(msg);
                                        self.current_message_subject = Some(envelope.subject.clone());
                                        self.message_scroll = 0;
                                        self.focus = Focus::Message;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(Action::None)
    }

    fn draw(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),    // Main content
                Constraint::Length(1), // Footer/Help
            ])
            .split(f.size());

        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(20),
                Constraint::Percentage(80),
            ])
            .split(chunks[0]);

        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Percentage(60),
            ])
            .split(main_chunks[1]);

        // Folders Pane
        let folders_title = Line::from(vec![
            Span::styled(" 1 ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::styled(" Folders ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        ]);
        let folders_block = Block::default()
            .borders(Borders::ALL)
            .title(folders_title)
            .border_style(Style::default().fg(if matches!(self.focus, Focus::Folders) { Color::Yellow } else { Color::White }));
        
        let folder_items: Vec<ListItem> = self.folders
            .iter()
            .map(|f| {
                let style = if f.name == self.current_folder {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Gray)
                };
                ListItem::new(Span::styled(f.name.as_str(), style))
            })
            .collect();
            
        let folders_list = List::new(folder_items)
            .block(folders_block)
            .highlight_style(Style::default().bg(Color::Rgb(60, 60, 60)).add_modifier(Modifier::BOLD));
        
        f.render_stateful_widget(folders_list, main_chunks[0], &mut self.folders_state);

        // Envelopes Pane
        let envelopes_title = Line::from(vec![
            Span::styled(" 2 ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::styled(format!(" Envelopes ({}) ", self.current_folder), Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" [Page {}] ", self.current_page + 1), Style::default().fg(Color::Magenta)),
        ]);
        
        let branding_title = Span::styled(" Himalaya-TUI ", Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD));

        // Find total messages for footer
        let total_messages = self.folders.iter()
            .find(|f| f.name == self.current_folder)
            .and_then(|f| f.desc.parse::<usize>().ok());

        let envelopes_footer = match total_messages {
            Some(total) => format!(" {} / {} ", self.envelopes.len(), total),
            None => format!(" {} ", self.envelopes.len()),
        };

        let envelopes_block = Block::default()
            .borders(Borders::ALL)
            .title(envelopes_title)
            .title(block::Title::from(branding_title).alignment(Alignment::Right))
            .title_bottom(Line::from(envelopes_footer).alignment(Alignment::Right))
            .border_style(Style::default().fg(if matches!(self.focus, Focus::Envelopes) { Color::Yellow } else { Color::White }));

        if self.envelopes.is_empty() {
            let empty_msg = Paragraph::new("No messages found in this folder.")
                .block(envelopes_block)
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(empty_msg, right_chunks[0]);
        } else {
            let header = Row::new(vec![
                Cell::from(Span::styled("Date & Time", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("Sender", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
                Cell::from(Span::styled("Subject", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            ])
            .style(Style::default().bg(Color::Rgb(40, 40, 40)))
            .height(1);

            let rows = self.envelopes
                .iter()
                .map(|e| {
                    let date_time = if e.date.len() >= 16 {
                        e.date[..16].replace('T', " ")
                    } else {
                        e.date.clone()
                    };

                    Row::new(vec![
                        Cell::from(Span::styled(date_time, Style::default().fg(Color::Rgb(150, 150, 150)))),
                        Cell::from(Span::styled(e.from.addr.clone(), Style::default().fg(Color::Green))),
                        Cell::from(Span::styled(e.subject.clone(), Style::default().fg(Color::White))),
                    ])
                });

            let table = Table::new(rows, [
                Constraint::Length(17),
                Constraint::Length(30),
                Constraint::Min(0),
            ])
            .header(header)
            .block(envelopes_block)
            .highlight_style(Style::default().bg(Color::Rgb(60, 60, 60)).add_modifier(Modifier::BOLD))
            .highlight_symbol(">> ");

            f.render_stateful_widget(table, right_chunks[0], &mut self.envelopes_state);
        }

        // Message Pane
        let mut message_title_spans = vec![
            Span::styled(" 3 ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::styled(" Message Preview ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        ];

        if let Some(subject) = &self.current_message_subject {
            message_title_spans.push(Span::styled(format!(" {} ", subject), Style::default().fg(Color::White).bg(Color::Magenta)));
        }

        let message_title = Line::from(message_title_spans);
        let message_block = Block::default()
            .borders(Borders::ALL)
            .title(message_title)
            .border_style(Style::default().fg(if matches!(self.focus, Focus::Message) { Color::Yellow } else { Color::White }));

        let message_text = match &self.current_message {
            Some(msg) => msg.as_str(),
            None => "\n  Select an email and press Enter to read.",
        };

        let message_paragraph = Paragraph::new(message_text)
            .block(message_block)
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false })
            .scroll((self.message_scroll, 0));

        f.render_widget(message_paragraph, right_chunks[1]);

        // Help bar & Version
        let help_text = " [1-3]: Panes | Enter: Read | n/p: Next/Prev Page | q: Quit | ?: Help | r: Reply | c: Compose | f: Forward | d: Download | R: Refresh ";
        let version_text = format!(" v{} ", env!("CARGO_PKG_VERSION"));
        
        let footer = Paragraph::new(Line::from(vec![
            Span::styled(help_text, Style::default().fg(Color::White)),
            Span::styled(format!("{: >width$}", version_text, width = f.size().width as usize - help_text.len()), Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC)),
        ])).style(Style::default().bg(Color::Rgb(50, 50, 50)));
        
        f.render_widget(footer, chunks[1]);

        // Help Popup
        if self.show_help {
            let area = self.centered_rect(60, 60, f.size());
            f.render_widget(Clear, area);
            let help_box = Block::default()
                .title(" Keyboard Shortcuts ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow));
            
            let help_content = vec![
                Line::from(vec![Span::styled("Navigation:", Style::default().add_modifier(Modifier::BOLD))]),
                Line::from("  1, 2, 3     : Switch to Folders (1), List (2) or Preview (3)"),
                Line::from("  Tab         : Cycle between panels"),
                Line::from("  j/k, Arrows : Scroll lists or message content"),
                Line::from("  Enter       : Select folder or Read email"),
                Line::from("  n / p       : Next / Previous Page"),
                Line::from(""),
                Line::from(vec![Span::styled("Actions:", Style::default().add_modifier(Modifier::BOLD))]),
                Line::from("  c           : Compose new email"),
                Line::from("  r           : Reply to selected email"),
                Line::from("  f           : Forward selected email"),
                Line::from("  d           : Download all attachments"),
                Line::from("  R           : Force refresh current view"),
                Line::from("  q           : Quit Himalaya"),
                Line::from(""),
                Line::from(vec![Span::styled("Press any key to close this help", Style::default().fg(Color::DarkGray))]),
            ];
            
            let help_paragraph = Paragraph::new(help_content)
                .block(help_box)
                .alignment(Alignment::Left)
                .wrap(Wrap { trim: true });
            f.render_widget(help_paragraph, area);
        }
    }

    fn centered_rect(&self, percent_x: u16, percent_y: u16, r: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ])
            .split(r);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ])
            .split(popup_layout[1])[1]
    }
}
