// Zero-dependency YAML parser with Bun.YAML fast path and pure-JS fallback
// (works identically under Bun and Node.js without any third-party packages).

export function splitFrontmatter(text) {
  const normalized = text.replace(/\r\n/g, "\n");
  if (!normalized.startsWith("---\n")) return null;
  const rest = normalized.slice(4);
  const endIdx = rest.search(/\n---(?:\n|$)/);
  if (endIdx < 0) return null;
  const frontmatter = rest.slice(0, endIdx);
  let body = rest.slice(endIdx + 4);
  if (body.startsWith("\n")) body = body.slice(1);
  return { frontmatter, body };
}

export function parseYaml(text, options = {}) {
  if (!options.forceJs && typeof globalThis.Bun !== "undefined" && globalThis.Bun?.YAML?.parse) {
    return globalThis.Bun.YAML.parse(text);
  }
  return parseYamlJs(text);
}

// ---------------------------------------------------------------- pure-JS YAML subset

function stripComment(line) {
  let inSingle = false;
  let inDouble = false;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (inSingle) {
      if (ch === "'") {
        if (line[i + 1] === "'") i++;
        else inSingle = false;
      }
      continue;
    }
    if (inDouble) {
      if (ch === "\\") {
        i++;
        continue;
      }
      if (ch === '"') inDouble = false;
      continue;
    }
    if (ch === "'") inSingle = true;
    else if (ch === '"') inDouble = true;
    else if (ch === "#" && (i === 0 || /\s/.test(line[i - 1]))) {
      return line.slice(0, i);
    }
  }
  return line;
}

function findUnquotedColon(str) {
  let inSingle = false;
  let inDouble = false;
  let braceDepth = 0;
  let bracketDepth = 0;
  for (let i = 0; i < str.length; i++) {
    const ch = str[i];
    if (inSingle) {
      if (ch === "'") {
        if (str[i + 1] === "'") i++;
        else inSingle = false;
      }
      continue;
    }
    if (inDouble) {
      if (ch === "\\") {
        i++;
        continue;
      }
      if (ch === '"') inDouble = false;
      continue;
    }
    if (ch === "'") inSingle = true;
    else if (ch === '"') inDouble = true;
    else if (ch === "{") braceDepth++;
    else if (ch === "}") braceDepth = Math.max(0, braceDepth - 1);
    else if (ch === "[") bracketDepth++;
    else if (ch === "]") bracketDepth = Math.max(0, bracketDepth - 1);
    else if (ch === ":" && braceDepth === 0 && bracketDepth === 0) {
      const next = str[i + 1];
      if (next === undefined || next === " " || next === "\t") {
        return i;
      }
    }
  }
  return -1;
}

function splitFlowItems(inner) {
  const items = [];
  let cur = "";
  let inSingle = false;
  let inDouble = false;
  let braceDepth = 0;
  let bracketDepth = 0;
  for (let i = 0; i < inner.length; i++) {
    const ch = inner[i];
    if (inSingle) {
      cur += ch;
      if (ch === "'") {
        if (inner[i + 1] === "'") {
          cur += inner[++i];
        } else {
          inSingle = false;
        }
      }
      continue;
    }
    if (inDouble) {
      cur += ch;
      if (ch === "\\" && i + 1 < inner.length) {
        cur += inner[++i];
        continue;
      }
      if (ch === '"') inDouble = false;
      continue;
    }
    if (ch === "'") {
      inSingle = true;
      cur += ch;
    } else if (ch === '"') {
      inDouble = true;
      cur += ch;
    } else if (ch === "{") {
      braceDepth++;
      cur += ch;
    } else if (ch === "}") {
      braceDepth--;
      cur += ch;
    } else if (ch === "[") {
      bracketDepth++;
      cur += ch;
    } else if (ch === "]") {
      bracketDepth--;
      cur += ch;
    } else if (ch === "," && braceDepth === 0 && bracketDepth === 0) {
      items.push(cur.trim());
      cur = "";
    } else {
      cur += ch;
    }
  }
  if (cur.trim() !== "") items.push(cur.trim());
  return items;
}

function parseScalar(raw) {
  const trimmed = raw.trim();
  if (trimmed === "" || trimmed === "null" || trimmed === "~") return null;
  if (trimmed === "true") return true;
  if (trimmed === "false") return false;
  if (trimmed.startsWith("'") && trimmed.endsWith("'") && trimmed.length >= 2) {
    return trimmed.slice(1, -1).replace(/''/g, "'");
  }
  if (trimmed.startsWith('"') && trimmed.endsWith('"') && trimmed.length >= 2) {
    try {
      return JSON.parse(trimmed);
    } catch {
      return trimmed.slice(1, -1);
    }
  }
  if (trimmed.startsWith("[") && trimmed.endsWith("]")) {
    const inner = trimmed.slice(1, -1).trim();
    if (inner === "") return [];
    return splitFlowItems(inner).map((item) => parseScalar(item));
  }
  if (trimmed.startsWith("{") && trimmed.endsWith("}")) {
    const inner = trimmed.slice(1, -1).trim();
    if (inner === "") return {};
    const obj = {};
    for (const entry of splitFlowItems(inner)) {
      const colonIdx = findUnquotedColon(entry);
      if (colonIdx < 0) {
        throw new Error(`Invalid flow mapping entry: ${entry}`);
      }
      const key = String(parseScalar(entry.slice(0, colonIdx)));
      const valStr = entry.slice(colonIdx + 1).trim();
      obj[key] = parseScalar(valStr);
    }
    return obj;
  }
  if (/^-?(?:0|[1-9]\d*)$/.test(trimmed)) {
    const num = Number(trimmed);
    if (Number.isSafeInteger(num)) return num;
  }
  if (/^-?(?:0|[1-9]\d*)\.\d+(?:[eE][+-]?\d+)?$/.test(trimmed)) {
    const num = Number(trimmed);
    if (Number.isFinite(num)) return num;
  }
  return trimmed;
}

function parseYamlJs(text) {
  const rawLines = text.replace(/\r\n/g, "\n").split("\n");
  const lines = [];
  for (let i = 0; i < rawLines.length; i++) {
    const line = rawLines[i];
    if (line.trim() === "---" || line.trim() === "...") continue;
    const stripped = stripComment(line);
    if (stripped.trim() === "") continue;
    const indent = stripped.length - stripped.trimStart().length;
    lines.push({ indent, text: stripped.trim(), raw: line, lineNo: i + 1 });
  }
  if (lines.length === 0) return null;

  let pos = 0;

  function parseBlockScalar(parentIndent, header) {
    const folded = header.startsWith(">");
    const chomp = header.endsWith("-") ? "strip" : header.endsWith("+") ? "keep" : "clip";
    const startRawIdx = lines[pos - 1].lineNo; // 1-based next line index in rawLines
    const collected = [];
    let blockIndent = null;
    let rawIdx = startRawIdx;
    while (rawIdx < rawLines.length) {
      const rl = rawLines[rawIdx];
      if (rl.trim() === "") {
        collected.push("");
        rawIdx++;
        continue;
      }
      const ind = rl.length - rl.trimStart().length;
      if (ind <= parentIndent) break;
      if (blockIndent === null) blockIndent = ind;
      collected.push(rl.slice(Math.min(blockIndent, ind)));
      rawIdx++;
    }
    // Advance `pos` past any pre-tokenized lines consumed by this block scalar.
    while (pos < lines.length && lines[pos].lineNo <= rawIdx) {
      pos++;
    }
    while (chomp !== "keep" && collected.length > 0 && collected[collected.length - 1] === "") {
      collected.pop();
    }
    let result = folded ? collected.join(" ") : collected.join("\n");
    if (chomp === "clip" && result.length > 0) result += "\n";
    return result;
  }

  function parseNode(minIndent) {
    if (pos >= lines.length) return null;
    const cur = lines[pos];
    if (cur.indent < minIndent) return null;
    if (cur.text === "-" || cur.text.startsWith("- ")) {
      return parseSequence(cur.indent);
    }
    if (findUnquotedColon(cur.text) >= 0) {
      return parseMapping(cur.indent);
    }
    pos++;
    return parseScalar(cur.text);
  }

  function parseSequence(seqIndent) {
    const arr = [];
    while (pos < lines.length) {
      const cur = lines[pos];
      if (cur.indent < seqIndent) break;
      if (cur.indent > seqIndent) {
        throw new Error(`Unexpected indentation at line ${cur.lineNo}`);
      }
      if (!(cur.text === "-" || cur.text.startsWith("- "))) break;
      const afterDash = cur.text === "-" ? "" : cur.text.slice(2).trim();
      pos++;
      if (afterDash === "") {
        if (pos < lines.length && lines[pos].indent > seqIndent) {
          arr.push(parseNode(lines[pos].indent));
        } else {
          arr.push(null);
        }
        continue;
      }
      if (/^[|>][+-]?$/.test(afterDash)) {
        arr.push(parseBlockScalar(seqIndent, afterDash));
        continue;
      }
      const colonIdx = findUnquotedColon(afterDash);
      if (colonIdx >= 0 && !afterDash.startsWith("{") && !afterDash.startsWith("[")) {
        // Inline mapping entry starting on the `- ` line.
        const itemIndent = seqIndent + 2;
        const obj = {};
        const key = String(parseScalar(afterDash.slice(0, colonIdx)));
        const valStr = afterDash.slice(colonIdx + 1).trim();
        if (valStr === "") {
          if (pos < lines.length && lines[pos].indent > itemIndent) {
            obj[key] = parseNode(lines[pos].indent);
          } else {
            obj[key] = null;
          }
        } else if (/^[|>][+-]?$/.test(valStr)) {
          obj[key] = parseBlockScalar(itemIndent, valStr);
        } else {
          obj[key] = parseScalar(valStr);
        }
        // Continuation keys at indent >= itemIndent (same indent as key after `- `).
        if (pos < lines.length && lines[pos].indent >= itemIndent) {
          const contIndent = lines[pos].indent;
          const rest = parseMapping(contIndent);
          Object.assign(obj, rest);
        }
        arr.push(obj);
        continue;
      }
      arr.push(parseScalar(afterDash));
    }
    return arr;
  }

  function parseMapping(mapIndent) {
    const obj = {};
    while (pos < lines.length) {
      const cur = lines[pos];
      if (cur.indent < mapIndent) break;
      if (cur.indent > mapIndent) {
        throw new Error(`Unexpected indentation at line ${cur.lineNo}: ${cur.text}`);
      }
      if (cur.text === "-" || cur.text.startsWith("- ")) break;
      const colonIdx = findUnquotedColon(cur.text);
      if (colonIdx < 0) {
        throw new Error(`Expected key: value mapping at line ${cur.lineNo}: ${cur.text}`);
      }
      const key = String(parseScalar(cur.text.slice(0, colonIdx)));
      const valStr = cur.text.slice(colonIdx + 1).trim();
      pos++;
      if (valStr === "") {
        if (pos < lines.length && lines[pos].indent > mapIndent) {
          obj[key] = parseNode(lines[pos].indent);
        } else if (
          pos < lines.length &&
          lines[pos].indent === mapIndent &&
          (lines[pos].text === "-" || lines[pos].text.startsWith("- "))
        ) {
          // Sequence at same indent as mapping key (valid YAML style).
          obj[key] = parseSequence(mapIndent);
        } else {
          obj[key] = null;
        }
      } else if (/^[|>][+-]?$/.test(valStr)) {
        obj[key] = parseBlockScalar(mapIndent, valStr);
      } else {
        obj[key] = parseScalar(valStr);
      }
    }
    return obj;
  }

  return parseNode(lines[0].indent);
}
