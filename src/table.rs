use console::{style, Term};

const COLORS: [fn(&str) -> console::StyledObject<&str>; 6] = [
    |s| style(s).yellow(),
    |s| style(s).green(),
    |s| style(s).blue(),
    |s| style(s).magenta(),
    |s| style(s).cyan(),
    |s| style(s).red(),
];

pub fn draw_table(table_header: String, headers: Vec<String>, content: Vec<Vec<String>>) {
    if content.is_empty() {
        println!("{} No data to display.", style("ℹ️").dim());
        return;
    }

    // Get terminal width, default to 80 if unavailable
    let terminal_width = Term::stdout().size().1 as usize;
    let max_width = if terminal_width > 10 { terminal_width } else { 80 };

    // Calculate column widths intelligently
    let num_cols = headers.len();
    if num_cols == 0 {
        return;
    }

    // Start with header lengths as minimum widths
    let mut col_widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();

    // Check content to determine optimal column widths
    for row in &content {
        for (i, cell) in row.iter().enumerate() {
            if i < col_widths.len() {
                col_widths[i] = col_widths[i].max(cell.len());
            }
        }
    }

    // Calculate total width needed (including separators)
    let separator_space = (num_cols - 1) * 2; // 2 spaces between columns
    let total_content_width: usize = col_widths.iter().sum::<usize>() + separator_space;

    // If content is too wide, proportionally reduce column widths
    if total_content_width > max_width - 4 { // Leave some margin
        let available_width = max_width - 4 - separator_space;
        let scale_factor = available_width as f64 / col_widths.iter().sum::<usize>() as f64;
        
        for width in &mut col_widths {
            *width = (*width as f64 * scale_factor).max(4.0) as usize; // Minimum 4 chars per column
        }
    }

    // Calculate the final content width (columns + separators)
    let final_content_width = col_widths.iter().sum::<usize>() + separator_space;
    
    // Calculate bar lengths:
    // - Header bar: longest of table header or final content width
    // - Column separator bar: final content width only
    let header_bar_length = table_header.len().max(final_content_width);
    let column_separator_length = final_content_width;

    // Draw table header with separator bar
    println!("{}", style(&table_header).bold());
    println!("{}", "━".repeat(header_bar_length));

    // Draw column headers
    let mut header_line = String::new();
    for (i, header) in headers.iter().enumerate() {
        if i > 0 {
            header_line.push_str("  ");
        }
        header_line.push_str(&format!("{:<width$}", 
            style(header).bold().cyan(), 
            width = col_widths[i]
        ));
    }
    println!("{}", header_line);

    // Draw separator line under headers
    println!("{}", "-".repeat(column_separator_length));

    // Draw content rows
    for row in &content {
        let mut row_line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i >= col_widths.len() {
                break;
            }
            
            if i > 0 {
                row_line.push_str("  ");
            }

            // Truncate cell content if it exceeds column width
            let display_cell = if cell.len() > col_widths[i] && col_widths[i] > 3 {
                format!("{}...", &cell[..col_widths[i] - 3])
            } else if cell.len() > col_widths[i] {
                cell[..col_widths[i]].to_string()
            } else {
                cell.clone()
            };

            // Apply color cycling
            let colored_cell = COLORS[i % COLORS.len()](&display_cell);
            row_line.push_str(&format!("{:<width$}", colored_cell, width = col_widths[i]));
        }
        println!("{}", row_line);
    }
}