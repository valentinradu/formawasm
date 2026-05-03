/*
 * highlight.js language definition for FormaLang source.
 *
 * The book examples are formalang `.fv` source — the language
 * formawasm consumes — so we register the same syntax here. Loaded
 * via `additional-js` in book.toml; registers itself with the
 * `hljs` global mdBook ships.
 *
 * Wired colors live in theme/custom.css under the .hljs-* classes.
 */

(function () {
    function formalang(hljs) {
        const KEYWORDS = {
            keyword: [
                "pub", "let", "mut", "sink", "fn", "extern",
                "struct", "trait", "impl", "enum", "mod", "use",
                "match", "for", "in", "if", "else",
                "inline", "no_inline", "cold",
                "as", "self",
            ].join(" "),
            type: [
                "String", "I32", "I64", "F32", "F64",
                "Boolean", "Path", "Regex", "Never", "Self",
            ].join(" "),
            literal: ["true", "false", "nil"].join(" "),
        };

        // ───── Comments ─────
        const LINE_DOC_COMMENT = {
            className: "comment",
            variants: [
                { begin: /\/\/!.*/ },
                { begin: /\/\/\/.*/ },
            ],
            relevance: 0,
        };
        const LINE_COMMENT = hljs.COMMENT("//", "$", { relevance: 0 });
        const BLOCK_COMMENT = hljs.COMMENT("/\\*", "\\*/", { relevance: 0 });

        // ───── Strings ─────
        const STRING_TRIPLE = {
            className: "string",
            begin: /"""/,
            end: /"""/,
            contains: [hljs.BACKSLASH_ESCAPE],
        };
        const STRING = {
            className: "string",
            begin: /"/,
            end: /"/,
            illegal: /\n/,
            contains: [hljs.BACKSLASH_ESCAPE],
        };

        // ───── Numbers (with optional I32/I64/F32/F64 suffix) ─────
        const NUMBER = {
            className: "number",
            begin:
                /\b[0-9][0-9_]*(\.[0-9][0-9_]*)?([eE][+-]?[0-9]+)?(I32|I64|F32|F64)?\b/,
            relevance: 0,
        };

        // ───── Regex literal: r/pattern/flags ─────
        const REGEX_LITERAL = {
            className: "regexp",
            begin: /\br\/(?:[^\/\\\n]|\\.)+\/[gimsuvy]*/,
            relevance: 10,
        };

        // ───── Path literal: /foo/bar.svg ─────
        const PATH_LITERAL = {
            className: "string",
            begin: /(?:^|(?<=[\s=:,(\[]))\/[a-zA-Z._~][^\s\\,(){}\[\]]*/,
            relevance: 5,
        };

        // ───── Function definition: fn name<...>(...) ─────
        const FUNCTION = {
            className: "function",
            beginKeywords: "fn",
            end: /[(<]/,
            excludeEnd: true,
            contains: [
                {
                    className: "title",
                    begin: /[a-zA-Z][a-zA-Z0-9_]*/,
                    endsParent: true,
                },
            ],
        };

        // ───── Type / struct / enum / trait declaration head ─────
        const TYPE_DECLARATION = {
            className: "class",
            beginKeywords: "struct trait enum impl",
            end: /[<{:\s]/,
            excludeEnd: true,
            contains: [
                {
                    className: "title",
                    begin: /[A-Z][a-zA-Z0-9_]*/,
                    endsParent: true,
                },
            ],
        };

        // ───── Capitalised identifier → type reference ─────
        const TYPE_REFERENCE = {
            className: "type",
            begin: /\b[A-Z][a-zA-Z0-9_]*\b/,
            relevance: 0,
        };

        return {
            name: "FormaLang",
            aliases: ["fv", "formalang"],
            keywords: KEYWORDS,
            contains: [
                LINE_DOC_COMMENT,
                LINE_COMMENT,
                BLOCK_COMMENT,
                STRING_TRIPLE,
                STRING,
                REGEX_LITERAL,
                PATH_LITERAL,
                NUMBER,
                FUNCTION,
                TYPE_DECLARATION,
                TYPE_REFERENCE,
                {
                    className: "symbol",
                    begin: /\.[a-z_][a-zA-Z0-9_]*/,
                    relevance: 0,
                },
            ],
        };
    }

    if (typeof hljs !== "undefined") {
        hljs.registerLanguage("formalang", formalang);
        hljs.registerLanguage("fv", formalang);
        document
            .querySelectorAll('code.language-formalang, code.language-fv')
            .forEach(function (el) {
                el.dataset.highlighted = "";
                el.classList.remove("hljs");
                hljs.highlightElement(el);
            });
    }
})();
