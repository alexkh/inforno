#{
    language: "C",
    extensions: [".c", ".h"],
    rules: [
        // 1. Comments
        #{ token: "comment", regex: `//[^\n]*|/\*.*?\*/` },
        
        // 2. Preprocessor Directives
        #{ token: "special", regex: `#[ \t]*include[ \t]*(?:<[^>]*>|"[^"]*")` },
        #{ token: "special", regex: `#[ \t]*[a-zA-Z_]+\b` },
        
        // 3. Strings and Chars
        #{ token: "string",  regex: `"[^"\\]*(?:\\.[^"\\]*)*"|'[^'\\]*(?:\\.[^'\\]*)*'` },
        
        // 4. Data Types 
        #{ token: "type",    regex: `\b(char|const|double|float|int|long|short|signed|unsigned|void)\b` },
        
        // 5. Keywords 
        #{ token: "keyword", regex: `\b(auto|break|case|continue|default|do|else|enum|extern|for|goto|if|register|return|sizeof|static|struct|switch|typedef|union|volatile|while|_Packed)\b` },
        
        // 6. Function Calls - NEW LOOKAHEAD LOGIC
        // This will only trigger if the engine peeks ahead and sees a parenthesis
        #{ token: "function", regex: `[a-zA-Z_][a-zA-Z0-9_]*`, followed_by: "(" },
        
        // 7. Numerics
        #{ token: "numeric", regex: `\b0[xX][0-9a-fA-F]+\b|\b\d+(?:\.\d+)?(?:[fFlL])?\b` },
        
        // 8. Standard Variables / Identifiers (THE CATCH-ALL)
        // If a word wasn't followed by a '(', it falls through to here
        #{ token: "literal", regex: `[a-zA-Z_][a-zA-Z0-9_]*` },
        
        // 9. Punctuation
        #{ token: "punctuation", regex: `[{}()\[\];,.*+\-/%&|^!=<>~?]` }
    ]
}

