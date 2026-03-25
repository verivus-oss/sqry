#!/usr/bin/env node
const args = process.argv.slice(2);

function emitQuery() {
  const result = {
    symbols: [
      {
        name: "process",
        qualified_name: "Service.process",
        kind: "Method",
        file_path: "tests/fixtures/sample.cpp",
        start_line: 42,
        language: "cpp"
      }
    ],
    text_matches: []
  };
  process.stdout.write(JSON.stringify(result));
}

function emitSearch() {
  const events = [
    {
      match: {
        path: "tests/fixtures/sample.cpp",
        line: 10,
        line_text: "helper();"
      }
    },
    {
      match: {
        path: "tests/fixtures/sample.cpp",
        line: 20,
        line_text: "helper();"
      }
    },
    {
      summary: {
        total: 2
      }
    }
  ];

  for (const event of events) {
    process.stdout.write(`${JSON.stringify(event)}\n`);
  }
}

switch (args[0]) {
  case "query":
    emitQuery();
    break;
  case "search":
    emitSearch();
    break;
  case "index":
    // mimic success
    break;
  default:
    process.stderr.write(`Unsupported command: ${args.join(" ")}`);
    process.exitCode = 1;
}
