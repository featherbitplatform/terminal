//! Visual check of the terminal widget.
//!
//! Shows the widget fed with a fixed sample: a truecolor gradient, the 16 ANSI colors as
//! foreground and background, text attributes, wide characters, an OSC 8 link, a selection and the
//! cursor in each shape (focused block in the main view; beam, underline and an unfocused hollow
//! block below it).
//!
//!   cargo run -p fbterm-term --example widget_demo
//!   cargo run -p fbterm-term --example widget_demo -- --screenshot out.png
//!
//! `--screenshot` captures this window only (iced `window::screenshot`), writes a PNG and exits.

use std::sync::Arc;
use std::time::Duration;

use fbterm_config::Rgb;
use fbterm_term::{
    ColorScheme, GridSize, Selection, SharedTerminal, Terminal, TerminalOutput, TerminalStyle,
    terminal_view,
};
use feathercritty::index::{Column, Line, Point};
use iced::widget::{column, container, row, text};
use iced::window::Screenshot;
use iced::{Element, Font, Length, Size, Subscription, Task};
use parking_lot::Mutex;

const MAIN_SAMPLE: &str = concat!(
    "\x1b[1mfbterm widget demo\x1b[0m\r\n",
    "\r\n",
    "ansi fg  ",
    "\x1b[30m 0 \x1b[31m 1 \x1b[32m 2 \x1b[33m 3 \x1b[34m 4 \x1b[35m 5 \x1b[36m 6 \x1b[37m 7 ",
    "\x1b[90m 8 \x1b[91m 9 \x1b[92m10 \x1b[93m11 \x1b[94m12 \x1b[95m13 \x1b[96m14 \x1b[97m15\x1b[0m\r\n",
    "ansi bg  ",
    "\x1b[40m 0 \x1b[41m 1 \x1b[42m 2 \x1b[43m 3 \x1b[44m 4 \x1b[45m 5 \x1b[46m 6 \x1b[47m 7 ",
    "\x1b[100m 8 \x1b[101m 9 \x1b[102m10 \x1b[103m11 \x1b[104m12 \x1b[105m13 \x1b[106m14 \x1b[107m15\x1b[0m\r\n",
    "\r\n",
    "styles   \x1b[1mbold\x1b[0m  \x1b[3mitalic\x1b[0m  \x1b[1;3mbold italic\x1b[0m  ",
    "\x1b[4munderline\x1b[0m  \x1b[4:2mdouble\x1b[0m  \x1b[9mstrikethrough\x1b[0m  ",
    "\x1b[2mdim\x1b[0m  \x1b[7minverse\x1b[0m  \x1b[4;58;2;165;132;255mcolored underline\x1b[0m\r\n",
    "colors   \x1b[31mred\x1b[0m \x1b[32mgreen\x1b[0m \x1b[33myellow\x1b[0m \x1b[34mblue\x1b[0m ",
    "\x1b[35mmagenta\x1b[0m \x1b[36mcyan\x1b[0m \x1b[38;5;208m256-color 208\x1b[0m ",
    "\x1b[38;2;255;105;180mtruecolor fg\x1b[0m \x1b[2;31mdim red\x1b[0m\r\n",
    "wide     日本語のテキスト | emoji 🚀 🙂 | after wide\r\n",
    "link     \x1b]8;;https://example.com/docs\x1b\\example.com/docs\x1b]8;;\x1b\\ (OSC 8, Cmd/Ctrl+click opens)\r\n",
    "select   the quick brown fox jumps over the lazy dog\r\n",
    "box      ┌──────┬──────┐ ▁▂▃▄▅▆▇█ ░▒▓\r\n",
    "         └──────┴──────┘\r\n",
    "\r\n",
    "\x1b[32muser@host\x1b[0m:\x1b[34m~/src/fbterm\x1b[0m$ ",
);

struct Pane {
    term: SharedTerminal,
    style: TerminalStyle,
    sample: String,
    fed: bool,
}

#[derive(Debug, Clone)]
enum Message {
    Term(usize, TerminalOutput),
    Shoot,
    Shot(Screenshot),
}

struct App {
    panes: Vec<Pane>,
    screenshot: Option<String>,
    shooting: bool,
}

fn gradient(cols: u16) -> String {
    let mut s = String::from("gradient ");
    let n = cols.saturating_sub(9).max(1);
    for i in 0..n {
        let t = f32::from(i) / f32::from(n);
        let r = (255.0 * (1.0 - t)) as u8;
        let g = (255.0 * (1.0 - (2.0 * t - 1.0).abs())) as u8;
        let b = (255.0 * t) as u8;
        s.push_str(&format!("\x1b[48;2;{r};{g};{b}m "));
    }
    s.push_str("\x1b[0m\r\n");
    s
}

fn pane(sample: String, focused: bool, accent: Option<Rgb>) -> Pane {
    let scheme = ColorScheme::graphite();
    let mut t = Terminal::new(
        GridSize {
            cols: 80,
            rows: 24,
            cell_width: 8.0,
            cell_height: 16.0,
        },
        1000,
    );
    scheme.apply_to(&mut t);
    Pane {
        term: Arc::new(Mutex::new(t)),
        style: TerminalStyle {
            scheme,
            font: Font::MONOSPACE,
            font_size: 14.0,
            accent,
            focused,
            background_opacity: 1.0,
        },
        sample,
        fed: false,
    }
}

/// Feed the sample once the widget has sized the terminal, so nothing reflows.
fn feed(p: &mut Pane, index: usize) {
    let mut t = p.term.lock();
    if index == 0 {
        let cols = t.size().cols;
        let (head, rest) = MAIN_SAMPLE.split_at(MAIN_SAMPLE.find("\r\n\r\n").unwrap() + 4);
        t.feed(head.as_bytes());
        t.feed(gradient(cols).as_bytes());
        t.feed(rest.as_bytes());
        // Select "quick brown fox" on the "select" line (row 10).
        let mut sel = Selection::start(Point::new(Line(10), Column(13)));
        sel.update(Point::new(Line(10), Column(27)));
        t.set_selection(Some(sel));
    } else {
        t.feed(p.sample.as_bytes());
    }
    p.fed = true;
}

fn boot(screenshot: Option<String>) -> (App, Task<Message>) {
    let violet = Rgb {
        r: 0xa5,
        g: 0x84,
        b: 0xff,
    };
    let panes = vec![
        pane(String::new(), true, Some(violet)),
        pane("\x1b[5 qbeam cursor: ".into(), true, None),
        pane("\x1b[3 qunderline cursor: ".into(), true, None),
        pane("\x1b[1 qunfocused (hollow): ".into(), false, None),
    ];
    (
        App {
            panes,
            screenshot,
            shooting: false,
        },
        Task::none(),
    )
}

fn update(app: &mut App, msg: Message) -> Task<Message> {
    match msg {
        Message::Term(i, TerminalOutput::Resized(size)) => {
            println!("pane {i} resized to {}x{}", size.cols, size.rows);
            if !app.panes[i].fed {
                feed(&mut app.panes[i], i);
            }
        }
        Message::Term(i, out) => println!("pane {i}: {out:?}"),
        Message::Shoot => {
            if !app.shooting {
                app.shooting = true;
                return iced::window::latest()
                    .and_then(iced::window::screenshot)
                    .map(Message::Shot);
            }
        }
        Message::Shot(shot) => {
            let path = app.screenshot.clone().expect("screenshot path");
            save_png(&path, &shot);
            println!(
                "saved {path} ({}x{} px, scale {})",
                shot.size.width, shot.size.height, shot.scale_factor
            );
            return iced::exit();
        }
    }
    Task::none()
}

fn save_png(path: &str, shot: &Screenshot) {
    let file = std::fs::File::create(path).expect("create screenshot file");
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        shot.size.width,
        shot.size.height,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("png header");
    w.write_image_data(&shot.rgba).expect("png data");
}

fn view(app: &App) -> Element<'_, Message> {
    let term = |i: usize| {
        let p = &app.panes[i];
        terminal_view(&p.term, &p.style, move |o| Message::Term(i, o))
    };
    let small = |i: usize| container(term(i)).width(Length::Fill).height(48);
    let label = |s: &'static str| {
        text(s)
            .size(12)
            .color(iced::Color::from_rgb8(0x91, 0x91, 0x99))
    };
    container(
        column![
            container(term(0)).height(Length::Fill),
            label("cursor shapes"),
            row![small(1), small(2), small(3)].spacing(8),
        ]
        .spacing(8),
    )
    .padding(12)
    .style(|_| container::Style {
        background: Some(iced::Color::from_rgb8(0x10, 0x10, 0x13).into()),
        ..container::Style::default()
    })
    .into()
}

fn subscription(app: &App) -> Subscription<Message> {
    if app.screenshot.is_some() && app.panes.iter().all(|p| p.fed) {
        // Give the renderer a moment to load fonts and present a frame.
        iced::time::every(Duration::from_millis(1500)).map(|_| Message::Shoot)
    } else {
        Subscription::none()
    }
}

fn main() -> iced::Result {
    let mut args = std::env::args().skip(1);
    let mut screenshot = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--screenshot" => screenshot = Some(args.next().expect("--screenshot <path.png>")),
            other => panic!("unknown argument {other}"),
        }
    }
    iced::application(move || boot(screenshot.clone()), update, view)
        .title("fbterm widget demo")
        .subscription(subscription)
        .theme(iced::Theme::Dark)
        .window_size(Size::new(1180.0, 560.0))
        .run()
}
