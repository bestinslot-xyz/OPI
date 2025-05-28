use std::error::Error;

use crate::config::{MAX_AMOUNT, MAX_DECIMALS};

fn is_positive_decimal(number: &str) -> bool {
    if number.is_empty() {
        return false;
    }
    let mut has_dot = false;
    let mut digits_after_dot = 0;
    for (i, c) in number.chars().enumerate() {
        if c == '.' {
            if i == 0 || i == number.len() - 1 {
                // Dot is at the start or end
                return false;
            }
            if has_dot {
                // More than one dot
                return false;
            }
            has_dot = true;
        } else if !c.is_digit(10) {
            // Not a digit
            return false;
        } else {
            if has_dot {
                digits_after_dot += 1;
            }
        }
    }
    if digits_after_dot > MAX_DECIMALS as usize {
        // Too many digits after the dot
        return false;
    }
    true
}

pub const ALLOW_ZERO: bool = true;
pub const DISALLOW_ZERO: bool = false;

pub fn get_amount_value(
    number: Option<&str>,
    ticker_decimals: u8,
    default_value: Option<u128>,
    allow_zero: bool,
) -> Result<u128, Box<dyn Error>> {
    let Some(number) = number else {
        if let Some(default_value) = default_value {
            return Ok(default_value);
        }
        return Err("Number required".into());
    };
    let mut result = String::new();
    if !is_positive_decimal(number) {
        return Err("Invalid number format".into());
    }
    if let Some(dot_index) = number.find('.') {
        let integer_part = &number[..dot_index];
        let decimal_part = &number[dot_index + 1..];
        result.push_str(integer_part);
        result.push_str(decimal_part);
        for _ in decimal_part.len()..18 as usize {
            result.push('0');
        }
    } else {
        result.push_str(number);
        if ticker_decimals > 0 {
            // No dot in the result
            // result.push('.');
            for _ in 0..18 as usize {
                result.push('0');
            }
        }
    }
    let uint = result.parse::<u128>()?;
    if uint > MAX_AMOUNT {
        return Err("Amount too large".into());
    }
    if uint == 0 && !allow_zero {
        return Err("Amount cannot be zero".into());
    }
    Ok(uint)
}

fn is_positive_integer(number: &str) -> bool {
    number.chars().all(|c| c.is_digit(10)) && !number.is_empty()
}

pub fn get_decimals_value(number: Option<&str>) -> Result<u8, Box<dyn Error>> {
    match number {
        Some(number) => {
            if !is_positive_integer(number) {
                return Err("Invalid `dec` format".into());
            }
            let decimals = number.parse::<u8>()?;
            if decimals > MAX_DECIMALS {
                return Err("`dec` too large".into());
            }
            Ok(decimals)
        }
        None => Ok(MAX_DECIMALS),
    }
}

#[macro_export]
macro_rules! default {
    ($value: expr) => {
        Some($value)
    };
}

#[macro_export]
macro_rules! no_default {
    () => {
        None
    };
}
