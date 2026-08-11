// SPDX-License-Identifier: MPL-2.0

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::fl;
use cosmic::Theme;
use cosmic::cosmic_config::{self, CosmicConfigEntry};
use cosmic::iced::futures::SinkExt;
use cosmic::iced::{Alignment, Length, Limits, Subscription, window::Id};
use cosmic::iced_winit::commands::popup::{destroy_popup, get_popup};
use cosmic::prelude::*;
use cosmic::widget::{self, container};

const BOLT_SVG: &[u8] = include_bytes!("../resources/bolt.svg");
const INFINITY_SVG: &[u8] = include_bytes!("../resources/infinity.svg");

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[derive(Default)]
pub struct AppModel {
    core: cosmic::Core,
    popup: Option<Id>,
    config: Config,
    config_handler: Option<cosmic_config::Config>,
    inhibit_conn: Option<zbus::blocking::Connection>,
    inhibit_cookie: Option<u32>,
}

impl Drop for AppModel {
    fn drop(&mut self) {
        // Release only this instance's inhibit. Deliberately does not touch the
        // shared config: one panel instance exiting must not turn Vigil off on
        // the others.
        self.release_inhibit();
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    ToggleVigil,
    Activate(u32),
    Deactivate,
    Tick,
    PopupClosed(Id),
    UpdateConfig(Config),
}

impl AppModel {
    /// Whether Vigil is on. Shared by every panel instance via config.
    fn active(&self) -> bool {
        self.config.active
    }

    /// Seconds left before expiry; zero when indefinite or inactive.
    ///
    /// Derived from the shared deadline rather than counted down locally, so
    /// each monitor's instance shows the same value.
    fn remaining_secs(&self) -> u32 {
        if !self.config.active || self.config.expiry_ts == 0 {
            return 0;
        }
        u32::try_from(self.config.expiry_ts.saturating_sub(now_secs())).unwrap_or(u32::MAX)
    }

    /// Take a screensaver inhibit for this instance. Does not touch config.
    ///
    /// Every instance holds its own inhibit. `cosmic-idle` keeps inhibitors in
    /// a list and only lets the screen sleep once all of them are gone, so the
    /// duplication is harmless and avoids electing a single owner process.
    fn acquire_inhibit(&mut self) -> bool {
        if self.inhibit_cookie.is_some() {
            return true;
        }

        let conn = match zbus::blocking::Connection::session() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to connect to session bus: {e}");
                return false;
            }
        };

        let reply = conn.call_method(
            Some("org.freedesktop.ScreenSaver"),
            "/org/freedesktop/ScreenSaver",
            Some("org.freedesktop.ScreenSaver"),
            "Inhibit",
            &("Vigil", "User requested screen stay awake"),
        );

        match reply {
            Ok(msg) => match msg.body().deserialize::<u32>() {
                Ok(cookie) => {
                    self.inhibit_conn = Some(conn);
                    self.inhibit_cookie = Some(cookie);
                    true
                }
                Err(e) => {
                    eprintln!("failed to parse Inhibit reply: {e}");
                    false
                }
            },
            Err(e) => {
                eprintln!("failed to call ScreenSaver.Inhibit: {e}");
                false
            }
        }
    }

    /// Drop this instance's inhibit. Does not touch config.
    fn release_inhibit(&mut self) {
        if let (Some(conn), Some(cookie)) = (self.inhibit_conn.take(), self.inhibit_cookie.take()) {
            let _ = conn.call_method(
                Some("org.freedesktop.ScreenSaver"),
                "/org/freedesktop/ScreenSaver",
                Some("org.freedesktop.ScreenSaver"),
                "UnInhibit",
                &(cookie,),
            );
        }
    }

    /// Bring this instance's inhibit in line with the shared flag.
    ///
    /// Used when another monitor's instance toggled Vigil. Never writes config,
    /// which is what stops two instances from echoing updates back and forth.
    fn sync_inhibit(&mut self) {
        if self.config.active {
            self.acquire_inhibit();
        } else {
            self.release_inhibit();
        }
    }

    /// Turn Vigil on in response to a user action, and publish that to the
    /// other instances.
    fn activate(&mut self, duration_mins: u32) {
        if !self.acquire_inhibit() {
            return;
        }
        self.config.duration_mins = duration_mins;
        self.config.active = true;
        self.config.expiry_ts = if duration_mins == 0 {
            0
        } else {
            now_secs() + u64::from(duration_mins) * 60
        };
        self.save_config();
    }

    /// Turn Vigil off in response to a user action or expiry, and publish that
    /// to the other instances.
    fn deactivate(&mut self) {
        self.release_inhibit();
        self.config.active = false;
        self.config.expiry_ts = 0;
        self.save_config();
    }

    fn is_indefinite(&self) -> bool {
        self.active() && self.config.duration_mins == 0
    }

    fn format_remaining(&self) -> String {
        if self.is_indefinite() {
            "\u{221e}".to_string()
        } else {
            let mins = self.remaining_secs().div_ceil(60);
            format!("{mins}")
        }
    }

    fn format_remaining_full(&self) -> String {
        if self.is_indefinite() {
            fl!("indefinite")
        } else {
            let secs = self.remaining_secs();
            format!("{:02}:{:02}", secs / 60, secs % 60)
        }
    }

    fn save_config(&self) {
        if let Some(ref handler) = self.config_handler {
            let _ = self.config.write_entry(handler);
        }
    }

    fn active_color() -> cosmic::iced::Color {
        cosmic::iced::Color::from_rgb(0.96, 0.76, 0.07)
    }

    fn active_color_muted() -> cosmic::iced::Color {
        let c = Self::active_color();
        cosmic::iced::Color::from_rgba(c.r, c.g, c.b, 0.3)
    }
}

fn colored_bg(color: cosmic::iced::Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_theme: &Theme| container::Style {
        background: Some(color.into()),
        border: cosmic::iced::Border {
            radius: radius.into(),
            ..Default::default()
        },
        ..container::Style::default()
    }
}

impl cosmic::Application for AppModel {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;

    const APP_ID: &'static str = "com.github.bgub.CosmicExtAppletVigil";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(
        core: cosmic::Core,
        _flags: Self::Flags,
    ) -> (Self, Task<cosmic::Action<Self::Message>>) {
        let (config, config_handler) =
            match cosmic_config::Config::new(Self::APP_ID, Config::VERSION) {
                Ok(handler) => {
                    let config = match Config::get_entry(&handler) {
                        Ok(config) => config,
                        Err((_errors, config)) => config,
                    };
                    (config, Some(handler))
                }
                Err(_) => (Config::default(), None),
            };

        let mut app = AppModel {
            core,
            config,
            config_handler,
            popup: None,
            inhibit_conn: None,
            inhibit_cookie: None,
        };

        // Adopt whatever state is already shared. This covers an instance
        // starting late (a monitor plugged in while Vigil is on) and
        // `cosmic-panel` respawning an applet it killed.
        if app.config.active {
            if app.config.expiry_ts != 0 && now_secs() >= app.config.expiry_ts {
                app.deactivate();
            } else {
                app.sync_inhibit();
            }
        }

        (app, Task::none())
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn view(&self) -> Element<'_, Self::Message> {
        let (_, panel_h) = self.core.applet.suggested_window_size();
        #[allow(clippy::cast_precision_loss)]
        let diameter = panel_h.get() as f32;
        let radius = diameter / 2.0;

        let icon = widget::icon(widget::icon::from_svg_bytes(BOLT_SVG).symbolic(true))
            .width(Length::Fixed(18.0))
            .height(Length::Fixed(18.0));

        let bg_color = if self.active() {
            Self::active_color_muted()
        } else {
            cosmic::iced::Color::TRANSPARENT
        };

        let mut row = widget::row().push(icon).align_y(Alignment::Center);

        if self.active() {
            if self.is_indefinite() {
                row = row.push(
                    widget::icon(widget::icon::from_svg_bytes(INFINITY_SVG).symbolic(true))
                        .width(Length::Fixed(14.0))
                        .height(Length::Fixed(7.0)),
                );
            } else {
                row = row.push(widget::text(self.format_remaining()).size(14.0));
            }
            row = row.spacing(4);
        }

        let active = self.active();
        let pill_height = diameter * 0.8;
        let pill_radius = pill_height / 2.0;
        let content = widget::container(row)
            .height(Length::Fixed(if active { pill_height } else { diameter }))
            .width(if active {
                Length::Shrink
            } else {
                Length::Fixed(diameter)
            })
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .padding(if active {
                [0.0, pill_radius / 2.0]
            } else {
                [0.0, 0.0]
            })
            .style(move |_theme: &Theme| {
                let r = if active { pill_radius } else { radius };
                container::Style {
                    background: Some(bg_color.into()),
                    border: cosmic::iced::Border {
                        radius: r.into(),
                        ..Default::default()
                    },
                    ..container::Style::default()
                }
            });

        let btn = widget::button::custom(self.core.applet.autosize_window(content))
            .on_press(Message::ToggleVigil)
            .class(cosmic::theme::Button::AppletIcon);

        widget::mouse_area(btn)
            .on_right_release(Message::TogglePopup)
            .into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Self::Message> {
        let status_text = if self.active() {
            fl!("active")
        } else {
            fl!("inactive")
        };

        let status_color = if self.active() {
            Self::active_color_muted()
        } else {
            cosmic::iced::Color::from_rgba(1.0, 1.0, 1.0, 0.08)
        };

        let status_row = widget::row()
            .push(widget::text::heading(status_text))
            .push(widget::space().width(Length::Fill))
            .push_maybe(if self.active() {
                Some(widget::text::heading(self.format_remaining_full()))
            } else {
                None
            })
            .align_y(Alignment::Center);

        let status_block = widget::container(status_row)
            .width(Length::Fill)
            .padding([10, 16])
            .style(colored_bg(status_color, 12.0));

        // Duration preset buttons
        let active_mins = if self.active() {
            Some(self.config.duration_mins)
        } else {
            None
        };
        let dur_btn = |mins: u32, label: &'static str| {
            if active_mins == Some(mins) {
                widget::button::suggested(label)
                    .on_press(Message::Deactivate)
                    .width(Length::Fill)
            } else {
                widget::button::standard(label)
                    .on_press(Message::Activate(mins))
                    .width(Length::Fill)
            }
        };

        let infinity_msg = if active_mins == Some(0) {
            Message::Deactivate
        } else {
            Message::Activate(0)
        };
        let infinity_class = if active_mins == Some(0) {
            cosmic::theme::Button::Suggested
        } else {
            cosmic::theme::Button::Standard
        };
        let infinity_btn = widget::button::custom(
            widget::text("\u{221e}").size(26.0).line_height(0.8).center(),
        )
        .padding([6, 12])
        .on_press(infinity_msg)
        .width(Length::Shrink)
        .class(infinity_class);

        let duration_row = widget::row()
            .push(dur_btn(15, "15m"))
            .push(dur_btn(30, "30m"))
            .push(dur_btn(60, "1h"))
            .push(dur_btn(120, "2h"))
            .push(infinity_btn)
            .spacing(6)
            .align_y(Alignment::Center);

        let content = widget::column()
            .push(status_block)
            .push(widget::text::heading(fl!("duration")))
            .push(duration_row)
            .spacing(12)
            .align_x(Alignment::Center)
            .padding(12);

        self.core.applet.popup_container(content).into()
    }

    fn subscription(&self) -> Subscription<Self::Message> {
        let mut subs = vec![
            self.core()
                .watch_config::<Config>(Self::APP_ID)
                .map(|update| Message::UpdateConfig(update.config)),
        ];

        if self.active() && !self.is_indefinite() {
            struct TimerTick;
            subs.push(Subscription::run_with(
                std::any::TypeId::of::<TimerTick>(),
                |_| {
                    cosmic::iced::stream::channel::<Message>(1, async |mut channel| {
                        loop {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            _ = channel.send(Message::Tick).await;
                        }
                    })
                },
            ));
        }

        Subscription::batch(subs)
    }

    fn update(&mut self, message: Self::Message) -> Task<cosmic::Action<Self::Message>> {
        match message {
            Message::ToggleVigil => {
                if self.active() {
                    self.deactivate();
                } else {
                    let duration = self.config.duration_mins;
                    self.activate(duration);
                }
            }
            Message::Activate(mins) => {
                self.activate(mins);
            }
            Message::Deactivate => {
                self.deactivate();
            }
            Message::Tick => {
                // Only drives the countdown redraw; the deadline itself lives
                // in config, so both instances expire at the same moment.
                if self.remaining_secs() == 0 {
                    self.deactivate();
                }
            }
            Message::UpdateConfig(config) => {
                self.config = config;
                self.sync_inhibit();
            }
            Message::TogglePopup => {
                return if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    let new_id = Id::unique();
                    self.popup.replace(new_id);
                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    popup_settings.positioner.size_limits = Limits::NONE
                        .max_width(372.0)
                        .min_width(300.0)
                        .min_height(100.0)
                        .max_height(400.0);
                    get_popup(popup_settings)
                };
            }
            Message::PopupClosed(id) => {
                if self.popup.as_ref() == Some(&id) {
                    self.popup = None;
                }
            }
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
