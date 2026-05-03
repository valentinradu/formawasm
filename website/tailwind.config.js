/** @type {import('tailwindcss').Config} */
module.exports = {
  content: ["./website/**/*.{html,js}"],
  theme: {
    extend: {
      colors: {
        // Tokyo Night — keep in sync with theme/css/variables.css.
        bg:           "#1a1b26",
        bgDark:       "#16161e",
        bgHighlight:  "#292e42",
        terminal:     "#15161e",
        fg:           "#c0caf5",
        fgDark:       "#a9b1d6",
        fgGutter:     "#3b4261",
        dark3:        "#545c7e",
        comment:      "#565f89",
        dark5:        "#737aa2",
        blue0:        "#3d59a1",
        blue:         "#7aa2f7",
        cyan:         "#7dcfff",
        blue1:        "#2ac3de",
        blue2:        "#0db9d7",
        blue5:        "#89ddff",
        blue6:        "#b4f9f8",
        blue7:        "#394b70",
        magenta:      "#bb9af7",
        purple:       "#9d7cd8",
        orange:       "#ff9e64",
        yellow:       "#e0af68",
        green:        "#9ece6a",
        green1:       "#73daca",
        teal:         "#1abc9c",
        red:          "#f7768e",
      },
      fontFamily: {
        sans: ['"Inter"', "ui-sans-serif", "system-ui", "sans-serif"],
        mono: ['"JetBrains Mono"', "ui-monospace", "SFMono-Regular", "monospace"],
      },
    },
  },
  plugins: [],
};
