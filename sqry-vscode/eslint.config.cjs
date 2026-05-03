const tsParser = require("@typescript-eslint/parser");
const tsPlugin = require("@typescript-eslint/eslint-plugin");

const wireKeyPattern =
  "^(?:[a-z][a-z0-9]*(?:_[a-z0-9]+)+|[A-Z][A-Z0-9_]*|[A-Za-z]+(?:-[A-Za-z]+)+|sqry\\.statusBar\\.[a-z]+|Activating|LspStarting|WorkspaceResolving|Ready|Failed)$";

module.exports = [
  {
    ignores: ["out/**", "dist/**", "**/*.d.ts"],
  },
  {
    files: ["src/**/*.ts"],
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        ecmaVersion: 2022,
        sourceType: "module",
      },
    },
    plugins: {
      "@typescript-eslint": tsPlugin,
    },
    rules: {
      "@typescript-eslint/naming-convention": [
        "warn",
        {
          selector: "default",
          format: ["camelCase"],
        },
        {
          selector: "typeLike",
          format: ["PascalCase"],
        },
        {
          selector: "variable",
          format: ["camelCase", "UPPER_CASE"],
        },
        {
          selector: "parameter",
          format: null,
          filter: {
            regex: "^_$",
            match: true,
          },
        },
        {
          selector: "parameter",
          format: ["camelCase"],
          leadingUnderscore: "allow",
        },
        {
          selector: "classProperty",
          format: ["camelCase"],
          leadingUnderscore: "allow",
        },
        {
          selector: ["objectLiteralProperty", "typeProperty"],
          format: null,
          filter: {
            regex: wireKeyPattern,
            match: true,
          },
        },
        {
          selector: ["objectLiteralProperty", "typeProperty"],
          format: ["camelCase"],
        },
      ],
      curly: "warn",
      eqeqeq: "warn",
      "no-throw-literal": "warn",
      semi: "warn",
    },
  },
];
