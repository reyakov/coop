use chrono::{Local, TimeZone};
use gpui::SharedString;
use nostr_sdk::prelude::*;

const NOW: &str = "now";
const SECONDS_IN_MINUTE: i64 = 60;
const MINUTES_IN_HOUR: i64 = 60;
const HOURS_IN_DAY: i64 = 24;
const DAYS_IN_MONTH: i64 = 30;

pub trait TimestampExt {
    fn to_human_time(&self) -> SharedString;
    fn to_ago(&self) -> SharedString;
}

impl TimestampExt for Timestamp {
    fn to_human_time(&self) -> SharedString {
        let input_time = match Local.timestamp_opt(self.as_secs() as i64, 0) {
            chrono::LocalResult::Single(time) => time,
            _ => return SharedString::from("9999"),
        };

        let now = Local::now();
        let input_date = input_time.date_naive();
        let now_date = now.date_naive();
        let yesterday_date = (now - chrono::Duration::days(1)).date_naive();
        let time_format = input_time.format("%H:%M %p");

        match input_date {
            date if date == now_date => SharedString::from(format!("Today at {time_format}")),
            date if date == yesterday_date => {
                SharedString::from(format!("Yesterday at {time_format}"))
            }
            _ => SharedString::from(format!("{}, {time_format}", input_time.format("%d/%m/%y"))),
        }
    }

    fn to_ago(&self) -> SharedString {
        let input_time = match Local.timestamp_opt(self.as_secs() as i64, 0) {
            chrono::LocalResult::Single(time) => time,
            _ => return SharedString::from("1m"),
        };

        let now = Local::now();
        let duration = now.signed_duration_since(input_time);

        match duration {
            d if d.num_seconds() < SECONDS_IN_MINUTE => SharedString::from(NOW),
            d if d.num_minutes() < MINUTES_IN_HOUR => {
                SharedString::from(format!("{}m", d.num_minutes()))
            }
            d if d.num_hours() < HOURS_IN_DAY => SharedString::from(format!("{}h", d.num_hours())),
            d if d.num_days() < DAYS_IN_MONTH => SharedString::from(format!("{}d", d.num_days())),
            _ => SharedString::from(input_time.format("%b %d").to_string()),
        }
    }
}
