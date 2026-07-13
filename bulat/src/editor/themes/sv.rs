use super::ColorTheme;

impl ColorTheme {
    pub const SV: ColorTheme = ColorTheme {
        name: "Sv",
        dark: true, // Set to false if it's a light theme

        // From Step 1 (Global Colors)
        bg: "#000000",        // editor.background
        cursor: "#d29922",    // editorCursor.foreground
        selection: "#0c2d6b", // editor.selectionBackground

        // From Step 2 (Inspector Tool)
        comments: "#00bb00",    // Click on // comment
        functions: "#00ddff",   // Click on fn name()
        keywords: "#bb0000",    // Click on 'pub', 'fn', 'let'
        literals: "#cccc66",    // Click on variable name
        numerics: "#4499ff",    // Click on number
        punctuation: "#ff6666", // Click on ; or {
        strs: "#ce9178",        // Click on "string"
        types: "#4ec9b0",       // Click on String or u32
        special: "#c586c0",     // Click on 'lifetime or true/false
    };
}
