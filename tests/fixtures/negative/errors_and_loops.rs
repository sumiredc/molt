// Result, Option, ?, for loops, ranges and while / loop with labels.
use std::num::ParseIntError;

fn parse_all(lines: &Vec<String>) -> Result<Vec<u32>, ParseIntError> {
    let mut numbers = Vec::new();
    for line in lines {
        let n: u32 = line.trim().parse()?;
        numbers.push(n);
    }
    Ok(numbers)
}

fn first_even(numbers: &Vec<u32>) -> Option<u32> {
    for i in 0..numbers.len() {
        if numbers[i] % 2 == 0 {
            return Some(numbers[i]);
        }
    }
    None
}

fn countdown(mut n: u32) -> u32 {
    let mut steps = 0;
    'outer: loop {
        while n > 0 {
            n -= 1;
            steps += 1;
            if steps > 100 {
                break 'outer;
            }
        }
        break;
    }
    steps
}

fn describe(value: Option<u32>) -> String {
    match value {
        Some(n) if n > 10 => format!("big {n}"),
        Some(n) => format!("small {n}"),
        None => String::new(),
    }
}
