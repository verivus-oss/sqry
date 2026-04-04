// @ts-nocheck
// Graph visualization for sqry webview panel.
// Runs in VS Code webview context — no Node.js APIs available.
(function() {
  const vscode = acquireVsCodeApi();
  const container = document.getElementById("graph-container");
  const searchInput = document.getElementById("search");
  const exportBtn = document.getElementById("export-btn");
  const statusDiv = document.getElementById("status");

  let svgElement = null;
  const transform = { x: 0, y: 0, scale: 1 };

  // Handle messages from extension
  window.addEventListener("message", function(event) {
    const message = event.data;
    switch (message.type) {
      case "graphData":
        renderGraph(message.nodes, message.edges, message.truncated, message.totalNodes, message.totalEdges);
        break;
      case "error":
        showError(message.message);
        break;
    }
  });

  function showError(text) {
    // Safe DOM construction — no raw HTML injection
    while (container.firstChild) {
      container.firstChild.remove();
    }
    const div = document.createElement("div");
    div.className = "error";
    div.textContent = text;
    container.appendChild(div);
  }

  function renderGraph(nodes, edges, truncated, totalNodes, totalEdges) {
    if (nodes.length === 0) {
      showError("No graph data to display");
      return;
    }

    // Simple layout: grid-based positioning
    const CELL_W = 180;
    const CELL_H = 60;
    const PADDING = 20;
    const cols = Math.max(1, Math.ceil(Math.sqrt(nodes.length)));

    const positioned = nodes.map(function(node, i) {
      return {
        id: node.id,
        label: node.label,
        kind: node.kind,
        file: node.file,
        line: node.line,
        language: node.language,
        x: PADDING + (i % cols) * CELL_W,
        y: PADDING + Math.floor(i / cols) * CELL_H,
        width: CELL_W - 10,
        height: CELL_H - 10,
      };
    });

    const nodeMap = new Map();
    for (const p of positioned) {
      nodeMap.set(p.id, p);
    }
    const totalW = PADDING * 2 + cols * CELL_W;
    const totalH = PADDING * 2 + Math.ceil(nodes.length / cols) * CELL_H;

    // Build SVG
    const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("width", "100%");
    svg.setAttribute("height", "100%");
    svg.setAttribute("viewBox", "0 0 " + totalW + " " + totalH);

    // Arrow marker
    const defs = document.createElementNS("http://www.w3.org/2000/svg", "defs");
    const marker = document.createElementNS("http://www.w3.org/2000/svg", "marker");
    marker.setAttribute("id", "arrow");
    marker.setAttribute("viewBox", "0 0 10 10");
    marker.setAttribute("refX", "10");
    marker.setAttribute("refY", "5");
    marker.setAttribute("markerWidth", "8");
    marker.setAttribute("markerHeight", "8");
    marker.setAttribute("orient", "auto-start-reverse");
    marker.setAttribute("class", "edge");
    const arrowPath = document.createElementNS("http://www.w3.org/2000/svg", "path");
    arrowPath.setAttribute("d", "M 0 0 L 10 5 L 0 10 z");
    arrowPath.setAttribute("fill", "var(--vscode-editorWidget-border)");
    marker.appendChild(arrowPath);
    defs.appendChild(marker);
    svg.appendChild(defs);

    const g = document.createElementNS("http://www.w3.org/2000/svg", "g");

    // Draw edges
    for (const edge of edges) {
      const src = nodeMap.get(edge.source);
      const tgt = nodeMap.get(edge.target);
      if (!src || !tgt) continue;

      const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      const sx = src.x + src.width / 2;
      const sy = src.y + src.height;
      const tx = tgt.x + tgt.width / 2;
      const ty = tgt.y;
      path.setAttribute("d", "M " + sx + " " + sy + " C " + sx + " " + (sy + 20) + ", " + tx + " " + (ty - 20) + ", " + tx + " " + ty);
      path.setAttribute("class", "edge");
      path.setAttribute("marker-end", "url(#arrow)");
      const edgeG = document.createElementNS("http://www.w3.org/2000/svg", "g");
      edgeG.setAttribute("class", "edge");
      edgeG.appendChild(path);
      g.appendChild(edgeG);
    }

    // Draw nodes
    for (const node of positioned) {
      const nodeG = document.createElementNS("http://www.w3.org/2000/svg", "g");
      nodeG.setAttribute("class", "node");
      nodeG.setAttribute("tabindex", "0");
      nodeG.setAttribute("role", "button");
      nodeG.setAttribute("aria-label", node.label + " (" + (node.kind || "symbol") + ") - click to navigate");

      const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
      title.textContent = node.label + "\n" + (node.file || "") + (node.line === undefined ? "" : ":" + (node.line + 1));
      nodeG.appendChild(title);

      const rect = document.createElementNS("http://www.w3.org/2000/svg", "rect");
      rect.setAttribute("x", String(node.x));
      rect.setAttribute("y", String(node.y));
      rect.setAttribute("width", String(node.width));
      rect.setAttribute("height", String(node.height));
      rect.setAttribute("rx", "4");
      nodeG.appendChild(rect);

      const text = document.createElementNS("http://www.w3.org/2000/svg", "text");
      text.setAttribute("x", String(node.x + node.width / 2));
      text.setAttribute("y", String(node.y + node.height / 2 + 4));
      text.setAttribute("text-anchor", "middle");
      const label = node.label.length > 20 ? node.label.slice(0, 18) + "\u2026" : node.label;
      text.textContent = label;
      nodeG.appendChild(text);

      // Click to navigate — use IIFE to capture node in closure
      (function(capturedNode) {
        nodeG.addEventListener("click", function() {
          if (capturedNode.file) {
            vscode.postMessage({ type: "navigateToFile", file: capturedNode.file, line: capturedNode.line || 0 });
          }
        });
        nodeG.addEventListener("keydown", function(e) {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            if (capturedNode.file) {
              vscode.postMessage({ type: "navigateToFile", file: capturedNode.file, line: capturedNode.line || 0 });
            }
          }
        });
      })(node);

      g.appendChild(nodeG);
    }

    svg.appendChild(g);

    // Clear container safely, then append SVG
    while (container.firstChild) {
      container.firstChild.remove();
    }
    container.appendChild(svg);
    svgElement = svg;

    // Status
    let statusText = nodes.length + " nodes, " + edges.length + " edges";
    if (truncated) {
      statusText += " (truncated from " + totalNodes + " nodes, " + totalEdges + " edges)";
    }
    statusDiv.textContent = statusText;

    // Pan/zoom
    setupPanZoom(svg, g);
  }

  function setupPanZoom(svg, g) {
    let isPanning = false;
    let startX = 0;
    let startY = 0;

    svg.addEventListener("wheel", function(e) {
      e.preventDefault();
      const delta = e.deltaY > 0 ? 0.9 : 1.1;
      transform.scale *= delta;
      transform.scale = Math.max(0.1, Math.min(5, transform.scale));
      g.setAttribute("transform", "translate(" + transform.x + "," + transform.y + ") scale(" + transform.scale + ")");
    });

    svg.addEventListener("mousedown", function(e) {
      if (e.target.closest(".node")) return;
      isPanning = true;
      startX = e.clientX - transform.x;
      startY = e.clientY - transform.y;
    });

    globalThis.addEventListener("mousemove", function(e) {
      if (!isPanning) return;
      transform.x = e.clientX - startX;
      transform.y = e.clientY - startY;
      g.setAttribute("transform", "translate(" + transform.x + "," + transform.y + ") scale(" + transform.scale + ")");
    });

    globalThis.addEventListener("mouseup", function() { isPanning = false; });
  }

  // Search
  searchInput.addEventListener("input", function() {
    const query = searchInput.value.toLowerCase();
    const nodes = container.querySelectorAll(".node");
    for (const nodeEl of nodes) {
      const textEl = nodeEl.querySelector("text");
      const label = textEl ? (textEl.textContent || "").toLowerCase() : "";
      nodeEl.style.opacity = !query || label.includes(query) ? "1" : "0.2";
    }
  });

  // Export SVG
  exportBtn.addEventListener("click", function() {
    if (!svgElement) return;
    const svgData = new XMLSerializer().serializeToString(svgElement);
    const blob = new Blob([svgData], { type: "image/svg+xml" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "sqry-graph.svg";
    a.click();
    URL.revokeObjectURL(url);
  });
})();
