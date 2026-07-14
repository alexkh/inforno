use similar::{Algorithm, DiffOp, TextDiff};

pub struct BulatEngine;

impl BulatEngine {
    /// Computes the diff ops between two strings using Patience algorithm
    pub fn compute_diffs(left: &str, right: &str) -> Vec<DiffOp> {
        let diff = TextDiff::configure()
            .algorithm(Algorithm::Patience)
            .diff_lines(left, right);
        diff.ops().to_vec()
    }

    /// Pure function: Applies a DiffOp from the right side onto the left side,
    /// returning the newly merged left string.
    pub fn apply_merge(left_code: &str, right_code: &str, op: &DiffOp) -> String {
        let left_lines: Vec<&str> = left_code.lines().collect();
        let right_lines: Vec<&str> = right_code.lines().collect();

        let mut new_left: Vec<String> = left_lines.iter().map(|s| s.to_string()).collect();

        match op {
            DiffOp::Equal { .. } => return left_code.to_string(),
            DiffOp::Delete { old_index, old_len, .. } => {
                new_left.drain(*old_index..*old_index + *old_len);
            }
            DiffOp::Insert { old_index, new_index, new_len } => {
                let text_to_insert: Vec<String> = right_lines[*new_index..*new_index + *new_len]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                new_left.splice(*old_index..*old_index, text_to_insert);
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                let text_to_insert: Vec<String> = right_lines[*new_index..*new_index + *new_len]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                new_left.splice(*old_index..*old_index + *old_len, text_to_insert);
            }
        }

        let mut final_string = new_left.join("\n");
        
        // Standard code editor behavior: ensure the file ends with a trailing newline
        if !final_string.ends_with('\n') && !final_string.is_empty() {
            final_string.push('\n');
        }

        final_string
    }

    /// Extracts the real code from a padded view by removing gap lines
    pub fn extract_real_code(view: &str) -> String {
        let mut real = String::new();
        let lines: Vec<&str> = view.split('\n').collect();

        let mut is_first = true;
        for line in lines {
            // If the line is exactly our invisible gap marker, ignore it
            if line == "\u{200B}" {
                continue;
            }

            if !is_first {
                real.push('\n');
            }
            // Strip the marker in case the user manually typed text INTO a gap line
            real.push_str(&line.replace('\u{200B}', ""));
            is_first = false;
        }
        real
    }
}
