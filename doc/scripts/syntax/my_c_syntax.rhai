#{
    language: "C",
    extensions: [".c", ".h"],
    rules: [
        #{ token: "comment", regex: `//[^\n]*|/\*.*?\*/` },
        #{ token: "string",  regex: `"[^"\\]*(?:\\.[^"\\]*)*"` },
        #{ token: "keyword", regex: `\b(int|char|void|return|if|else|while|for|switch|case|break|struct)\b` },
        #{ token: "numeric", regex: `\b\d+(?:\.\d+)?\b` },
        #{ token: "special", regex: `[{}()\[\];,]` }
    ]
}

