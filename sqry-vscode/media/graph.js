// @ts-nocheck
// Graph visualization for sqry webview panel.
// Runs in VS Code webview context — no Node.js APIs available.
(function() {
  var vscode = acquireVsCodeApi();
  var container = document.getElementById("graph-container");
  var searchInput = document.getElementById("search");
  var exportBtn = document.getElementById("export-btn");
  var statusDiv = document.getElementById("status");

  var svgElement = null;
  var transform = { x: 0, y: 0, scale: 1 };

  // Handle messages from extension
  window.addEventListener("message", function(event) {
    var message = event.data;
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
      container.removeChild(container.firstChild);
    }
    var div = document.createElement("div");
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
    var CELL_W = 180;
    var CELL_H = 60;
    var PADDING = 20;
    var cols = Math.max(1, Math.ceil(Math.sqrt(nodes.length)));

    var positioned = nodes.map(function(node, i) {
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

    var nodeMap = new Map();
    for (var k = 0; k < positioned.length; k++) {
      nodeMap.set(positioned[k].id, positioned[k]);
    }
    var totalW = PADDING * 2 + cols * CELL_W;
    var totalH = PADDING * 2 + Math.ceil(nodes.length / cols) * CELL_H;

    // Build SVG
    var svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
    svg.setAttribute("width", "100%");
    svg.setAttribute("height", "100%");
    svg.setAttribute("viewBox", "0 0 " + totalW + " " + totalH);

    // Arrow marker
    var defs = document.createElementNS("http://www.w3.org/2000/svg", "defs");
    var marker = document.createElementNS("http://www.w3.org/2000/svg", "marker");
    marker.setAttribute("id", "arrow");
    marker.setAttribute("viewBox", "0 0 10 10");
    marker.setAttribute("refX", "10");
    marker.setAttribute("refY", "5");
    marker.setAttribute("markerWidth", "8");
    marker.setAttribute("markerHeight", "8");
    marker.setAttribute("orient", "auto-start-reverse");
    marker.setAttribute("class", "edge");
    var arrowPath = document.createElementNS("http://www.w3.org/2000/svg", "path");
    arrowPath.setAttribute("d", "M 0 0 L 10 5 L 0 10 z");
    arrowPath.setAttribute("fill", "var(--vscode-editorWidget-border)");
    marker.appendChild(arrowPath);
    defs.appendChild(marker);
    svg.appendChild(defs);

    var g = document.createElementNS("http://www.w3.org/2000/svg", "g");

    // Draw edges
    for (var ei = 0; ei < edges.length; ei++) {
      var edge = edges[ei];
      var src = nodeMap.get(edge.source);
      var tgt = nodeMap.get(edge.target);
      if (!src || !tgt) continue;

      var path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      var sx = src.x + src.width / 2;
      var sy = src.y + src.height;
      var tx = tgt.x + tgt.width / 2;
      var ty = tgt.y;
      path.setAttribute("d", "M " + sx + " " + sy + " C " + sx + " " + (sy + 20) + ", " + tx + " " + (ty - 20) + ", " + tx + " " + ty);
      path.setAttribute("class", "edge");
      path.setAttribute("marker-end", "url(#arrow)");
      var edgeG = document.createElementNS("http://www.w3.org/2000/svg", "g");
      edgeG.setAttribute("class", "edge");
      edgeG.appendChild(path);
      g.appendChild(edgeG);
    }

    // Draw nodes
    for (var ni = 0; ni < positioned.length; ni++) {
      var node = positioned[ni];
      var nodeG = document.createElementNS("http://www.w3.org/2000/svg", "g");
      nodeG.setAttribute("class", "node");
      nodeG.setAttribute("tabindex", "0");
      nodeG.setAttribute("role", "button");
      nodeG.setAttribute("aria-label", node.label + " (" + (node.kind || "symbol") + ") - click to navigate");

      var title = document.createElementNS("http://www.w3.org/2000/svg", "title");
      title.textContent = node.label + "\n" + (node.file || "") + (node.line !== undefined ? ":" + (node.line + 1) : "");
      nodeG.appendChild(title);

      var rect = document.createElementNS("http://www.w3.org/2000/svg", "rect");
      rect.setAttribute("x", String(node.x));
      rect.setAttribute("y", String(node.y));
      rect.setAttribute("width", String(node.width));
      rect.setAttribute("height", String(node.height));
      rect.setAttribute("rx", "4");
      nodeG.appendChild(rect);

      var text = document.createElementNS("http://www.w3.org/2000/svg", "text");
      text.setAttribute("x", String(node.x + node.width / 2));
      text.setAttribute("y", String(node.y + node.height / 2 + 4));
      text.setAttribute("text-anchor", "middle");
      var label = node.label.length > 20 ? node.label.slice(0, 18) + "\u2026" : node.label;
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
      container.removeChild(container.firstChild);
    }
    container.appendChild(svg);
    svgElement = svg;

    // Status
    var statusText = nodes.length + " nodes, " + edges.length + " edges";
    if (truncated) {
      statusText += " (truncated from " + totalNodes + " nodes, " + totalEdges + " edges)";
    }
    statusDiv.textContent = statusText;

    // Pan/zoom
    setupPanZoom(svg, g);
  }

  function setupPanZoom(svg, g) {
    var isPanning = false;
    var startX = 0;
    var startY = 0;

    svg.addEventListener("wheel", function(e) {
      e.preventDefault();
      var delta = e.deltaY > 0 ? 0.9 : 1.1;
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

    window.addEventListener("mousemove", function(e) {
      if (!isPanning) return;
      transform.x = e.clientX - startX;
      transform.y = e.clientY - startY;
      g.setAttribute("transform", "translate(" + transform.x + "," + transform.y + ") scale(" + transform.scale + ")");
    });

    window.addEventListener("mouseup", function() { isPanning = false; });
  }

  // Search
  searchInput.addEventListener("input", function() {
    var query = searchInput.value.toLowerCase();
    var nodes = container.querySelectorAll(".node");
    for (var i = 0; i < nodes.length; i++) {
      var nodeEl = nodes[i];
      var textEl = nodeEl.querySelector("text");
      var label = textEl ? (textEl.textContent || "").toLowerCase() : "";
      nodeEl.style.opacity = !query || label.indexOf(query) !== -1 ? "1" : "0.2";
    }
  });

  // Export SVG
  exportBtn.addEventListener("click", function() {
    if (!svgElement) return;
    var svgData = new XMLSerializer().serializeToString(svgElement);
    var blob = new Blob([svgData], { type: "image/svg+xml" });
    var url = URL.createObjectURL(blob);
    var a = document.createElement("a");
    a.href = url;
    a.download = "sqry-graph.svg";
    a.click();
    URL.revokeObjectURL(url);
  });
})();
