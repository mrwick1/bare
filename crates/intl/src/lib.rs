//! Locale data and formatting shared by the JavaScript engines.
#![deny(unsafe_code)]

pub mod icu;
pub mod intl;
pub mod display_names;
pub mod relative_time;
pub mod tz_names;
pub mod number_options;
mod intl_data;
mod intl_num_data;

mod number;
pub use number::number_to_string;
