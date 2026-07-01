// Tests for the webview message handler in media/graph.js.
//
// graph.js is a browser IIFE (no Node APIs, @ts-nocheck) so it cannot be
// imported directly. We load it into a vm sandbox backed by a minimal stub
// DOM, capture the "message" listener it registers on window, then dispatch
// synthetic MessageEvents to verify origin gating and payload validation.
//
// Observable signals:
//   - renderGraph (valid graphData, non-empty nodes) calls document.createElementNS
//     to build the SVG and never produces an error div.
//   - showError (valid error, or renderGraph on empty nodes) creates a <div>
//     whose className is set to "error".
//   - A rejected message (cross-origin, or malformed payload) touches neither.

import { expect } from "chai";
import * as fs from "fs";
import * as path from "path";
import * as vm from "vm";

const GRAPH_JS = path.join(__dirname, "..", "media", "graph.js");
const WEBVIEW_ORIGIN = "vscode-webview://test-guid";

interface Harness {
  dispatch: (event: { origin: string; data: unknown }) => void;
  nsCreated: () => number;
  errorDivs: () => string[];
  statusText: () => string;
}

/** Load graph.js into an isolated sandbox and return probes over its side effects. */
function loadGraph(origin: string): Harness {
  const src = fs.readFileSync(GRAPH_JS, "utf8");

  let nsCount = 0;
  const errorEls: any[] = [];
  const statusDiv = makeEl("div");

  function makeEl(tag: string): any {
    const el: any = {
      tag,
      children: [] as any[],
      parent: null as any,
      _class: "",
      textContent: "",
      style: {},
      get className() {
        return this._class;
      },
      set className(v: string) {
        this._class = v;
        if (v === "error") {
          // Record the element; textContent is assigned after className in
          // showError, so read it lazily in errorDivs().
          errorEls.push(this);
        }
      },
      get firstChild() {
        return this.children[0] || null;
      },
      appendChild(child: any) {
        child.parent = this;
        this.children.push(child);
        return child;
      },
      remove() {
        if (this.parent) {
          const i = this.parent.children.indexOf(this);
          if (i >= 0) {
            this.parent.children.splice(i, 1);
          }
          this.parent = null;
        }
      },
      setAttribute() {},
      addEventListener() {},
      querySelector() {
        return null;
      },
      querySelectorAll() {
        return [] as any[];
      },
    };
    return el;
  }

  const elementsById: Record<string, any> = {
    "graph-container": makeEl("div"),
    search: makeEl("input"),
    "export-btn": makeEl("button"),
    status: statusDiv,
  };

  const document = {
    getElementById: (id: string) => elementsById[id] ?? makeEl("div"),
    createElement: (tag: string) => makeEl(tag),
    createElementNS: (_ns: string, tag: string) => {
      nsCount += 1;
      return makeEl(tag);
    },
  };

  const messageListeners: Array<(e: any) => void> = [];

  const sandbox: any = {
    document,
    location: { origin },
    acquireVsCodeApi: () => ({ postMessage() {}, getState() {}, setState() {} }),
    addEventListener: (type: string, fn: (e: any) => void) => {
      if (type === "message") {
        messageListeners.push(fn);
      }
    },
    removeEventListener() {},
    XMLSerializer: class {},
    Blob: class {},
    URL: { createObjectURL: () => "", revokeObjectURL() {} },
    Math,
    Map,
    String,
    console,
  };

  vm.createContext(sandbox);
  // window and self must alias the sandbox global so window.addEventListener
  // and globalThis.location resolve to the stubs above.
  vm.runInContext("globalThis.window = globalThis; globalThis.self = globalThis;", sandbox);
  vm.runInContext(src, sandbox, { filename: "graph.js" });

  return {
    dispatch(event) {
      for (const fn of messageListeners) {
        fn(event);
      }
    },
    nsCreated: () => nsCount,
    errorDivs: () => errorEls.map((el) => el.textContent),
    statusText: () => statusDiv.textContent,
  };
}

const graphData = (over: Record<string, unknown> = {}) => ({
  type: "graphData",
  nodes: [{ id: "a", label: "alpha", file: "a.rs", line: 0 }],
  edges: [],
  ...over,
});

describe("graph.js webview message handler", () => {
  it("renders graphData delivered from the same origin", () => {
    const h = loadGraph(WEBVIEW_ORIGIN);
    h.dispatch({ origin: WEBVIEW_ORIGIN, data: graphData() });
    expect(h.nsCreated(), "SVG nodes should be built").to.be.greaterThan(0);
    expect(h.errorDivs(), "no error should be shown").to.deep.equal([]);
    expect(h.statusText()).to.contain("1 nodes");
  });

  it("shows an error message delivered from the same origin", () => {
    const h = loadGraph(WEBVIEW_ORIGIN);
    h.dispatch({ origin: WEBVIEW_ORIGIN, data: { type: "error", message: "boom" } });
    expect(h.errorDivs()).to.deep.equal(["boom"]);
    expect(h.nsCreated()).to.equal(0);
  });

  it("accepts messages with an empty origin (VS Code host channel)", () => {
    const h = loadGraph(WEBVIEW_ORIGIN);
    h.dispatch({ origin: "", data: graphData() });
    expect(h.nsCreated(), "empty-origin host messages must still render").to.be.greaterThan(0);
    expect(h.errorDivs()).to.deep.equal([]);
  });

  it("rejects messages from a mismatching cross-origin", () => {
    const h = loadGraph(WEBVIEW_ORIGIN);
    h.dispatch({ origin: "https://evil.example", data: graphData() });
    h.dispatch({ origin: "https://evil.example", data: { type: "error", message: "x" } });
    expect(h.nsCreated(), "cross-origin graphData must not render").to.equal(0);
    expect(h.errorDivs(), "cross-origin error must not display").to.deep.equal([]);
  });

  it("ignores malformed graphData payloads", () => {
    const cases: unknown[] = [
      null,
      undefined,
      "graphData",
      42,
      { type: "graphData" }, // missing nodes/edges
      { type: "graphData", nodes: "x", edges: [] }, // nodes not an array
      { type: "graphData", nodes: [], edges: {} }, // edges not an array
      { type: "unknown", nodes: [], edges: [] }, // wrong type
    ];
    for (const data of cases) {
      const h = loadGraph(WEBVIEW_ORIGIN);
      h.dispatch({ origin: WEBVIEW_ORIGIN, data });
      expect(h.nsCreated(), `payload ${JSON.stringify(data)} must not render`).to.equal(0);
      expect(h.errorDivs(), `payload ${JSON.stringify(data)} must not error`).to.deep.equal([]);
    }
  });

  it("ignores malformed error payloads", () => {
    const cases: unknown[] = [
      { type: "error" }, // missing message
      { type: "error", message: 123 }, // message not a string
      { type: "error", message: null },
    ];
    for (const data of cases) {
      const h = loadGraph(WEBVIEW_ORIGIN);
      h.dispatch({ origin: WEBVIEW_ORIGIN, data });
      expect(h.errorDivs(), `payload ${JSON.stringify(data)} must not display`).to.deep.equal([]);
      expect(h.nsCreated()).to.equal(0);
    }
  });

  it("shows the empty-graph error when nodes is an empty array", () => {
    const h = loadGraph(WEBVIEW_ORIGIN);
    h.dispatch({ origin: WEBVIEW_ORIGIN, data: graphData({ nodes: [] }) });
    // Valid graphData shape, but renderGraph reports empty via showError.
    expect(h.errorDivs()).to.deep.equal(["No graph data to display"]);
    expect(h.nsCreated()).to.equal(0);
  });
});
