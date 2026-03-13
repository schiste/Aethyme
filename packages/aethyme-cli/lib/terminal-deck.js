"use strict";

const CLEAR_SCREEN = "\x1b[2J\x1b[H";
const HIDE_CURSOR = "\x1b[?25l";
const SHOW_CURSOR = "\x1b[?25h";
const ANSI_PATTERN = /\x1b\[[0-9;]*m/g;

function repeat(char, count) {
  return char.repeat(Math.max(0, count));
}

function stripAnsi(text) {
  return String(text || "").replace(ANSI_PATTERN, "");
}

function visibleLength(text) {
  return stripAnsi(text).length;
}

function colorize(enabled, text, ...codes) {
  if (!enabled || codes.length === 0) {
    return text;
  }
  return `\x1b[${codes.join(";")}m${text}\x1b[0m`;
}

function pad(text, width) {
  const visible = visibleLength(text);
  if (visible >= width) {
    return stripAnsi(text).slice(0, width);
  }
  return `${text}${repeat(" ", width - visible)}`;
}

function center(text, width) {
  const plain = stripAnsi(text).slice(0, width);
  const left = Math.floor(Math.max(0, width - plain.length) / 2);
  const right = Math.max(0, width - plain.length - left);
  return `${repeat(" ", left)}${plain}${repeat(" ", right)}`;
}

function wrapText(text, width) {
  const lines = [];
  const paragraphs = String(text || "").replace(/\r/g, "").split("\n");

  for (const paragraph of paragraphs) {
    const words = paragraph.trim().split(/\s+/).filter(Boolean);
    if (words.length === 0) {
      lines.push("");
      continue;
    }

    let current = "";
    for (const word of words) {
      if (!current) {
        current = word;
      } else if (`${current} ${word}`.length <= width) {
        current = `${current} ${word}`;
      } else {
        lines.push(current);
        current = word;
      }
    }

    if (current) {
      lines.push(current);
    }
  }

  return lines;
}

function createTheme(enabled, paletteName = "default") {
  const palettes = {
    default: {
      deck: [30, 48, 1],
      badge: [30, 214, 1],
      kicker: [214, 1],
      title: [97, 1],
      subtitle: [252],
      rule: [45],
      dim: [244],
      label: [30, 51, 1],
      bullet: [214, 1],
      key: [81, 1],
      value: [231, 1],
      accent: [51, 1],
      success: [48, 1],
      warning: [220, 1],
      quote: [230, 3],
      terminalPrompt: [82, 1],
      terminalCmd: [231],
      terminalInfo: [117],
      terminalWarn: [220],
      terminalDim: [244],
      progressOn: [48, 1],
      progressOff: [238],
    },
    retro: {
      deck: [16, 220, 1],
      badge: [16, 178, 1],
      kicker: [220, 1],
      title: [229, 1],
      subtitle: [223],
      rule: [178],
      dim: [136],
      label: [16, 220, 1],
      bullet: [220, 1],
      key: [229, 1],
      value: [229, 1],
      accent: [214, 1],
      success: [190, 1],
      warning: [220, 1],
      quote: [229, 3],
      terminalPrompt: [220, 1],
      terminalCmd: [229],
      terminalInfo: [223],
      terminalWarn: [220],
      terminalDim: [136],
      progressOn: [220, 1],
      progressOff: [94],
    },
    signal: {
      deck: [16, 159, 1],
      badge: [16, 123, 1],
      kicker: [123, 1],
      title: [231, 1],
      subtitle: [195],
      rule: [123],
      dim: [110],
      label: [16, 159, 1],
      bullet: [123, 1],
      key: [159, 1],
      value: [231, 1],
      accent: [159, 1],
      success: [86, 1],
      warning: [221, 1],
      quote: [195, 3],
      terminalPrompt: [86, 1],
      terminalCmd: [231],
      terminalInfo: [159],
      terminalWarn: [221],
      terminalDim: [110],
      progressOn: [123, 1],
      progressOff: [24],
    },
  };

  const palette = palettes[paletteName] || palettes.default;
  return {
    enabled,
    deck: (text) => colorize(enabled, text, ...(palette.deck || [])),
    badge: (text) => colorize(enabled, text, ...(palette.badge || [])),
    kicker: (text) => colorize(enabled, text, ...(palette.kicker || [])),
    title: (text) => colorize(enabled, text, ...(palette.title || [])),
    subtitle: (text) => colorize(enabled, text, ...(palette.subtitle || [])),
    rule: (text) => colorize(enabled, text, ...(palette.rule || [])),
    dim: (text) => colorize(enabled, text, ...(palette.dim || [])),
    label: (text) => colorize(enabled, text, ...(palette.label || [])),
    bullet: (text) => colorize(enabled, text, ...(palette.bullet || [])),
    key: (text) => colorize(enabled, text, ...(palette.key || [])),
    value: (text) => colorize(enabled, text, ...(palette.value || [])),
    accent: (text) => colorize(enabled, text, ...(palette.accent || [])),
    success: (text) => colorize(enabled, text, ...(palette.success || [])),
    warning: (text) => colorize(enabled, text, ...(palette.warning || [])),
    quote: (text) => colorize(enabled, text, ...(palette.quote || [])),
    terminalPrompt: (text) => colorize(enabled, text, ...(palette.terminalPrompt || [])),
    terminalCmd: (text) => colorize(enabled, text, ...(palette.terminalCmd || [])),
    terminalInfo: (text) => colorize(enabled, text, ...(palette.terminalInfo || [])),
    terminalWarn: (text) => colorize(enabled, text, ...(palette.terminalWarn || [])),
    terminalDim: (text) => colorize(enabled, text, ...(palette.terminalDim || [])),
    progressOn: (text) => colorize(enabled, text, ...(palette.progressOn || [])),
    progressOff: (text) => colorize(enabled, text, ...(palette.progressOff || [])),
  };
}

function sectionStageCount(section) {
  if (section.reveal === "items" && Array.isArray(section.items)) {
    return section.items.length;
  }
  if (section.reveal === "lines" && Array.isArray(section.lines)) {
    return section.lines.length;
  }
  return 1;
}

function getSlideStageCount(slide) {
  return Math.max(
    slide.initialReveal || 1,
    (slide.sections || []).reduce((sum, section) => sum + sectionStageCount(section), 0)
  );
}

function revealSection(section, progress) {
  if (progress <= 0) {
    return null;
  }

  if (section.reveal === "items" && Array.isArray(section.items)) {
    return {
      ...section,
      items: section.items.slice(0, progress),
    };
  }

  if (section.reveal === "lines" && Array.isArray(section.lines)) {
    return {
      ...section,
      lines: section.lines.slice(0, progress),
    };
  }

  return section;
}

function getVisibleSections(slide, stage) {
  let remaining = stage;
  const visible = [];

  for (const section of slide.sections || []) {
    const stages = sectionStageCount(section);
    const sectionProgress = Math.min(remaining, stages);
    const revealed = revealSection(section, sectionProgress);
    if (revealed) {
      visible.push(revealed);
    }
    remaining -= stages;
  }

  return visible;
}

function renderBulletList(items, width, theme) {
  const lines = [];
  for (const item of items || []) {
    const wrapped = wrapText(item, Math.max(12, width - 4));
    wrapped.forEach((line, index) => {
      const prefix = index === 0 ? `${theme.bullet("*>")} ` : "   ";
      lines.push(`${prefix}${line}`);
    });
  }
  return lines;
}

function renderLead(text, width, theme) {
  return wrapText(text, width).map((line) => theme.subtitle(line));
}

function renderQuote(text, width, theme) {
  return wrapText(text, Math.max(12, width - 4)).map((line) => {
    return `${theme.bullet("||")} ${theme.quote(line)}`;
  });
}

function renderAscii(lines, width, theme, accent) {
  return (lines || []).map((line) => {
    const plain = String(line).slice(0, width);
    return accent ? theme.accent(plain) : plain;
  });
}

function renderMetrics(rows, width, theme) {
  const lines = [];
  for (const row of rows || []) {
    const metric = Array.isArray(row)
      ? { label: row[0], value: row[1], note: row[2], fill: row[3] }
      : row;
    const fill = typeof metric.fill === "number" ? Math.max(0, Math.min(1, metric.fill)) : null;
    lines.push(`${theme.key(metric.label)}`);
    if (fill === null) {
      wrapText(String(metric.value), Math.max(12, width - 4)).forEach((line) => {
        lines.push(`  ${theme.value(line)}`);
      });
    } else {
      const barWidth = Math.max(10, width - 18);
      const on = Math.round(barWidth * fill);
      const off = barWidth - on;
      const valueText = String(metric.value);
      const barText = `  [${theme.progressOn(repeat("#", on))}${theme.progressOff(repeat(".", off))}]`;
      if (visibleLength(barText) + 1 + valueText.length <= width) {
        lines.push(`${barText} ${theme.value(valueText)}`);
      } else {
        lines.push(barText);
        wrapText(valueText, Math.max(12, width - 4)).forEach((line) => {
          lines.push(`  ${theme.value(line)}`);
        });
      }
    }
    if (metric.note) {
      wrapText(metric.note, Math.max(12, width - 4)).forEach((line) => {
        lines.push(`  ${theme.dim(line)}`);
      });
    }
    lines.push("");
  }

  while (lines.length > 0 && lines[lines.length - 1] === "") {
    lines.pop();
  }

  return lines;
}

function renderTimeline(items, width, theme) {
  const lines = [];
  (items || []).forEach((item, index) => {
    const wrapped = wrapText(item, Math.max(12, width - 6));
    wrapped.forEach((line, lineIndex) => {
      const marker = lineIndex === 0 ? theme.bullet("o") : theme.dim("|");
      lines.push(` ${marker} ${line}`);
    });
    if (index < items.length - 1) {
      lines.push(` ${theme.dim("|")}`);
    }
  });
  return lines;
}

function renderTerminal(lines, width, theme) {
  return (lines || []).flatMap((entry) => {
    if (typeof entry === "string") {
      return wrapText(entry, width);
    }

    if (entry.kind === "command") {
      return wrapText(entry.text, Math.max(12, width - 4)).map((line, index) => {
        const prefix = index === 0 ? `${theme.terminalPrompt("$")} ` : "  ";
        return `${prefix}${theme.terminalCmd(line)}`;
      });
    }

    if (entry.kind === "info") {
      return wrapText(entry.text, Math.max(12, width - 4)).map((line) => `${theme.terminalInfo(">")} ${line}`);
    }

    if (entry.kind === "warn") {
      return wrapText(entry.text, Math.max(12, width - 4)).map((line) => `${theme.terminalWarn("!")} ${line}`);
    }

    if (entry.kind === "dim") {
      return wrapText(entry.text, Math.max(12, width - 2)).map((line) => theme.terminalDim(line));
    }

    return wrapText(String(entry.text || ""), width);
  });
}

function renderSectionContent(section, width, theme) {
  switch (section.kind) {
    case "lead":
      return renderLead(section.text, width, theme);
    case "bullets":
      return renderBulletList(section.items, width, theme);
    case "quote":
      return renderQuote(section.text, width, theme);
    case "ascii":
      return renderAscii(section.lines, width, theme, section.accent);
    case "metrics":
      return renderMetrics(section.rows, width, theme);
    case "timeline":
      return renderTimeline(section.items, width, theme);
    case "terminal":
      return renderTerminal(section.lines, width, theme);
    default:
      return wrapText(String(section.text || ""), width);
  }
}

function renderCard(section, width, theme) {
  const label = ` ${String(section.label || section.kind || "section").toUpperCase()} `;
  const inner = width - 4;
  const left = Math.floor(Math.max(0, width - label.length - 2) / 2);
  const right = Math.max(0, width - label.length - 2 - left);
  const top = `.${repeat("-", left)}${theme.label(label)}${repeat("-", right)}.`;
  const bottom = `'${repeat("-", width - 2)}'`;
  const content = renderSectionContent(section, inner, theme);
  const lines = [top];

  if (content.length === 0) {
    lines.push(`| ${repeat(" ", inner)} |`);
  } else {
    for (const line of content) {
      lines.push(`| ${pad(line, inner)} |`);
    }
  }

  lines.push(bottom);
  return lines;
}

function stackCards(sections, width, theme) {
  const lines = [];
  for (const section of sections) {
    lines.push(...renderCard(section, width, theme));
    lines.push("");
  }
  if (lines.length > 0) {
    lines.pop();
  }
  return lines;
}

function mergeColumns(leftLines, rightLines, width, gutter) {
  const leftWidth = Math.floor((width - gutter) / 2);
  const rightWidth = width - gutter - leftWidth;
  const height = Math.max(leftLines.length, rightLines.length);
  const lines = [];

  for (let i = 0; i < height; i += 1) {
    const left = pad(leftLines[i] || "", leftWidth);
    const right = pad(rightLines[i] || "", rightWidth);
    lines.push(`${left}${repeat(" ", gutter)}${right}`);
  }

  return lines;
}

function renderHeader(slide, width, theme) {
  const lines = [];

  if (slide.kicker) {
    lines.push(center(theme.kicker(slide.kicker.toUpperCase()), width));
    lines.push("");
  }

  if (slide.heroArt) {
    lines.push(...slide.heroArt.map((line) => center(theme.accent(line), width)));
    lines.push("");
  }

  lines.push(center(theme.title(slide.title), width));
  if (slide.subtitle) {
    lines.push(center(theme.subtitle(slide.subtitle), width));
  }
  lines.push(center(theme.rule(repeat("=", Math.min(46, width - 10))), width));
  lines.push("");

  return lines;
}

function renderBody(slide, sections, width, theme) {
  const variant = slide.variant || "standard";

  if (variant === "interstitial") {
    const lines = [];
    if (slide.interstitialArt) {
      lines.push(...slide.interstitialArt.map((line) => center(theme.accent(line), width)));
      lines.push("");
    }
    sections.forEach((section, index) => {
      const rendered = renderSectionContent(section, Math.min(width, 68), theme);
      rendered.forEach((line) => lines.push(center(line, width)));
      if (index < sections.length - 1) {
        lines.push("");
      }
    });
    return lines;
  }

  if (variant === "bigfact") {
    const lines = [];
    const first = sections[0];
    if (first) {
      const content = renderSectionContent(first, Math.min(width, 72), theme);
      content.forEach((line) => lines.push(center(line, width)));
      lines.push("");
    }
    const rest = sections.slice(1);
    if (rest.length > 0) {
      lines.push(...stackCards(rest, width, theme));
    }
    return lines;
  }

  if (variant === "split") {
    const leftSections = sections.filter((section) => (section.column || "left") === "left");
    const rightSections = sections.filter((section) => section.column === "right");
    const leftLines = stackCards(leftSections, Math.floor((width - 4) / 2), theme);
    const rightLines = stackCards(rightSections, width - 4 - Math.floor((width - 4) / 2), theme);
    return mergeColumns(leftLines, rightLines, width, 4);
  }

  return stackCards(sections, width, theme);
}

function buildProgress(current, total, width, theme) {
  const filled = Math.round((current / total) * width);
  return `${theme.progressOn(repeat("=", filled))}${theme.progressOff(repeat("-", width - filled))}`;
}

function renderFrame(deck, slideIndex, stage, terminalWidth = 100, terminalHeight = 30, colorEnabled = false) {
  const slide = deck.slides[slideIndex];
  const theme = createTheme(colorEnabled, slide.palette);
  const frameWidth = Math.min(Math.max(84, terminalWidth - 2), 112);
  const headerWidth = frameWidth;
  const bodyWidth = frameWidth - 4;
  const pageBadge = theme.badge(` ${slideIndex + 1}/${deck.slides.length} `);
  const deckLine = `${theme.deck(` ${deck.title} `)}${repeat(" ", Math.max(1, headerWidth - visibleLength(deck.title) - 6 - visibleLength(pageBadge)))}${pageBadge}`;
  const visibleSections = getVisibleSections(slide, stage);
  const body = [...renderHeader(slide, bodyWidth, theme), ...renderBody(slide, visibleSections, bodyWidth, theme)];
  const maxBodyHeight = Math.max(12, terminalHeight - 8);
  const cropped = body.slice(0, maxBodyHeight);

  while (cropped.length < maxBodyHeight) {
    cropped.push("");
  }

  const progressWidth = frameWidth < 96 ? 18 : 30;
  const progress = buildProgress(slideIndex + 1, deck.slides.length, progressWidth, theme);
  const revealBadge = theme.dim(`stage ${stage}/${getSlideStageCount(slide)}`);
  const footerHint =
    frameWidth < 96
      ? theme.dim("arrows next/back  q quit")
      : theme.dim("left/right reveal or move  g/G first/last  q quit");
  const footer = slide.hideFooter ? "" : `${progress}  ${footerHint}  ${revealBadge}`;
  const lines = [deckLine, theme.rule(repeat("=", frameWidth)), ""];

  cropped.forEach((line) => {
    lines.push(`  ${pad(line, bodyWidth)}`);
  });

  lines.push("");
  if (!slide.hideFooter) {
    lines.push(theme.rule(repeat("-", frameWidth)));
    lines.push(center(footer, frameWidth));
  }

  return lines.join("\n");
}

function getInitialStage(slide) {
  return Math.min(getSlideStageCount(slide), slide.initialReveal || 1);
}

function getFinalStage(slide) {
  return getSlideStageCount(slide);
}

function printDeck(deck) {
  const colorEnabled = Boolean(process.stdout.isTTY && !process.env.NO_COLOR);
  return (
    deck.slides
      .map((slide, index) =>
        renderFrame(deck, index, getFinalStage(slide), 112, 36, colorEnabled)
      )
      .join("\n\n") + "\n"
  );
}

function runTerminalDeck(deck) {
  if (!process.stdin.isTTY || !process.stdout.isTTY) {
    throw new Error("Interactive deck mode requires a TTY. Use --print instead.");
  }

  let slideIndex = 0;
  let stage = getInitialStage(deck.slides[0]);
  let closed = false;

  function cleanup() {
    if (closed) {
      return;
    }
    closed = true;
    process.stdout.write(SHOW_CURSOR);
    process.stdin.setRawMode(false);
    process.stdin.pause();
  }

  function render() {
    const width = process.stdout.columns || 100;
    const height = process.stdout.rows || 30;
    process.stdout.write(HIDE_CURSOR);
    process.stdout.write(CLEAR_SCREEN);
    process.stdout.write(renderFrame(deck, slideIndex, stage, width, height, !process.env.NO_COLOR));
    process.stdout.write("\n");
  }

  function next() {
    const currentSlide = deck.slides[slideIndex];
    if (stage < getFinalStage(currentSlide)) {
      stage += 1;
      return;
    }
    if (slideIndex < deck.slides.length - 1) {
      slideIndex += 1;
      stage = getInitialStage(deck.slides[slideIndex]);
    }
  }

  function previous() {
    const currentSlide = deck.slides[slideIndex];
    const initial = getInitialStage(currentSlide);
    if (stage > initial) {
      stage -= 1;
      return;
    }
    if (slideIndex > 0) {
      slideIndex -= 1;
      stage = getFinalStage(deck.slides[slideIndex]);
    }
  }

  function onData(buffer) {
    const key = String(buffer);

    if (key === "q" || key === "Q" || key === "\u0003") {
      cleanup();
      process.exit(0);
    }

    if (key === " " || key === "\r" || key === "\n" || key === "n" || key === "N" || key === "\u001b[C") {
      next();
      render();
      return;
    }

    if (key === "p" || key === "P" || key === "\u001b[D") {
      previous();
      render();
      return;
    }

    if (key === "g") {
      slideIndex = 0;
      stage = getInitialStage(deck.slides[0]);
      render();
      return;
    }

    if (key === "G") {
      slideIndex = deck.slides.length - 1;
      stage = getFinalStage(deck.slides[slideIndex]);
      render();
    }
  }

  process.stdin.setRawMode(true);
  process.stdin.resume();
  process.stdin.on("data", onData);
  process.stdout.on("resize", render);
  process.on("exit", cleanup);
  process.on("SIGINT", () => {
    cleanup();
    process.exit(130);
  });

  render();
}

module.exports = {
  printDeck,
  runTerminalDeck,
};
