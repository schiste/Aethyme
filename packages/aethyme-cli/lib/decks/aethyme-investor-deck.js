"use strict";

const aethymeWordmark = [
  "        ___      __  __",
  "       / _ |___ / /_/ /  __ _____  ___",
  "      / __ / -_) __/ _ \\/ // /  ' \\/ -_)",
  "     /_/ |_\\__/\\__/_//_/\\_, /_/_/_/\\__/",
  "                        /___/",
];

const retroPanel = [
  "    .==============================================.",
  "    |     AETHYME / CONTROL PLANE / SIGNAL v4      |",
  "    |----------------------------------------------|",
  "    |  MODELS GOT GOOD. OPERATIONS DID NOT.        |",
  "    '=============================================='",
];

const aethymeInvestorDeck = {
  title: "Aethyme Investor Deck",
  slides: [
    {
      kicker: "Aethyme",
      title: "The operating system for agentic development leads",
      subtitle: "Manage fleets of AI agents across real software environments",
      variant: "hero",
      palette: "signal",
      initialReveal: 1,
      heroArt: aethymeWordmark,
      sections: [
        {
          kind: "lead",
          label: "Founder signal",
          text:
            "Aethyme is what happens when a 20-year Wikimedia operator, former chair, revenue systems builder, and ultra-geek founder applies every discipline at once to software execution.",
        },
        {
          kind: "bullets",
          label: "Investment thesis",
          items: [
            "Future dev leads will manage teams of agents, not just people.",
            "They will need a management layer that spends less, diagnoses faster, and behaves more deterministically.",
          ],
        },
      ],
    },
    {
      kicker: "Why Now",
      title: "The bottleneck moved",
      subtitle: "From model quality to orchestration quality",
      variant: "interstitial",
      palette: "retro",
      initialReveal: 1,
      interstitialArt: retroPanel,
      sections: [
        {
          kind: "lead",
          text: "More agents do not automatically create more output.",
        },
        {
          kind: "lead",
          text: "They often create more token burn, more drift, and more managerial overhead.",
        },
      ],
    },
    {
      kicker: "Core Claim",
      title: "Agent sprawl is inevitable",
      subtitle: "The missing role is agentic dev lead. The missing layer is Aethyme.",
      variant: "bigfact",
      palette: "signal",
      initialReveal: 1,
      sections: [
        {
          kind: "lead",
          text:
            "Every serious engineering organization will need a control plane between frontier models and production software work.",
        },
        {
          kind: "bullets",
          label: "What that layer must do",
          items: [
            "Route attention to the right issue first.",
            "Reduce waste before models start spending hard.",
            "Make multi-agent execution observable and repeatable.",
          ],
        },
      ],
    },
    {
      kicker: "Problem",
      title: "Agentic dev work is expensive, noisy, and hard to steer",
      subtitle: "The wrong issue gets attention first",
      variant: "split",
      palette: "default",
      initialReveal: 1,
      sections: [
        {
          kind: "bullets",
          label: "Failure mode",
          column: "left",
          reveal: "items",
          items: [
            "Agents burn tokens on irrelevant context and weak retrieval paths.",
            "Runs drift toward the wrong issue because the system lacks stable structure for code, intent, and evidence.",
            "Leads need repeatability across backend, frontend, infra, docs, data, and legacy systems, not one-shot output.",
          ],
        },
        {
          kind: "terminal",
          label: "What it looks like",
          column: "right",
          lines: [
            { kind: "command", text: "run repair --repo monorepo --goal fix-auth" },
            { kind: "info", text: "Loaded 428 files into context window." },
            { kind: "warn", text: "Primary issue misidentified. Restarting with broader search." },
            { kind: "warn", text: "Token budget exceeded before root cause was isolated." },
            { kind: "dim", text: "Result: expensive motion, low determinism." },
          ],
        },
        {
          kind: "quote",
          label: "Shift",
          column: "right",
          text: "The management problem is becoming bigger than the raw model problem.",
        },
      ],
    },
    {
      kicker: "Product",
      title: "Aethyme turns software delivery into a structured system",
      subtitle: "Graph first. Route second. Spend last.",
      variant: "split",
      palette: "signal",
      initialReveal: 1,
      sections: [
        {
          kind: "bullets",
          label: "Control plane",
          column: "left",
          reveal: "items",
          items: [
            "Repository graphing and deterministic repo mapping create a stable substrate for agent work.",
            "Context packs and issue routing help agents fetch the right evidence before they spend.",
            "Eval loops and operator controls make multi-agent execution observable, steerable, and measurable.",
          ],
        },
        {
          kind: "ascii",
          label: "Pipeline",
          column: "right",
          accent: true,
          lines: [
            "[repo] -> [graph]",
            "              |",
            "              v",
            "       [context pack]",
            "              |",
            "              v",
            "       [issue routing]",
            "              |",
            "              v",
            "        [tools + evals]",
          ],
        },
        {
          kind: "terminal",
          label: "Operator view",
          column: "right",
          lines: [
            { kind: "command", text: "aethyme route --issue flaky-auth-test" },
            { kind: "info", text: "Likely root cause mapped to auth config, CI harness, and session fixtures." },
            { kind: "dim", text: "Context narrowed before the model spends." },
          ],
        },
      ],
    },
    {
      kicker: "System Edge",
      title: "This is not prompt engineering with better branding",
      subtitle: "It is cross-disciplinary systems design",
      variant: "split",
      palette: "default",
      initialReveal: 1,
      sections: [
        {
          kind: "lead",
          label: "Method",
          column: "left",
          text:
            "Aethyme pulls from knowledge organization, data structuration, librarianship, graph theory, neurobiology, and systems engineering.",
        },
        {
          kind: "timeline",
          label: "Why that matters",
          column: "right",
          items: [
            "Knowledge science improves retrieval and context selection.",
            "Graph structure exposes relationships hidden from linear prompting.",
            "Operational discipline makes agent behavior more deterministic and auditable.",
          ],
        },
      ],
    },
    {
      kicker: "Benchmarks",
      title: "The benchmark program is part of the product story",
      subtitle: "Real data slots cleanly into an already-defined proof frame",
      variant: "split",
      palette: "signal",
      initialReveal: 1,
      sections: [
        {
          kind: "metrics",
          label: "Benchmark slots",
          column: "left",
          rows: [
            {
              label: "Token reduction",
              value: "10% to 60% target",
              note: "Replace with measured median and spread later.",
              fill: 0.6,
            },
            {
              label: "Runtime delta",
              value: "Awaiting benchmark input",
              note: "Baseline agent runs vs routed Aethyme-assisted runs.",
              fill: 0.32,
            },
            {
              label: "Quality + variance",
              value: "Awaiting benchmark input",
              note: "Scored outputs plus run-to-run spread for the determinism thesis.",
              fill: 0.34,
            },
          ],
        },
        {
          kind: "terminal",
          label: "Measurement frame",
          column: "right",
          lines: [
            { kind: "command", text: "benchmark run --baseline --aethyme --same-task-set" },
            { kind: "info", text: "Input: same repos, same tasks, same success rubric." },
            { kind: "info", text: "Output: spend, latency, quality, and run-to-run variance." },
            { kind: "info", text: "Story: measurable operator leverage, not model hype." },
            { kind: "dim", text: "Your future benchmark data drops directly into this slide." },
          ],
        },
      ],
    },
    {
      kicker: "Buyer",
      title: "The buyer is the lead already accountable for delivery",
      subtitle: "The person who now has to manage agents as a system",
      variant: "split",
      palette: "default",
      initialReveal: 1,
      sections: [
        {
          kind: "bullets",
          label: "Ideal user",
          column: "left",
          reveal: "items",
          items: [
            "Engineering leads coordinating several AI agents across heterogeneous codebases and workflows.",
            "Teams that care about cost control, speed, answer quality, and predictable execution under pressure.",
            "Organizations moving from experimentation to managed agent operations.",
          ],
        },
        {
          kind: "ascii",
          label: "Need state",
          column: "right",
          lines: [
            "[more agents] != [more control]",
            "",
            "Need:",
            "  - routing",
            "  - observability",
            "  - repeatability",
            "  - operator leverage",
          ],
        },
      ],
    },
    {
      kicker: "Moat",
      title: "The moat is not the model, it is the operating layer",
      subtitle: "A compounding loop around real work",
      variant: "split",
      palette: "signal",
      initialReveal: 1,
      sections: [
        {
          kind: "timeline",
          label: "Compounding loop",
          column: "left",
          items: [
            "Graph-grounded repository intelligence compounds with every indexed environment.",
            "Evaluation data compounds into better routing, context assembly, and operator playbooks.",
            "The founder edge compounds because the product sits at the intersection of governance, knowledge systems, and deep technical execution.",
          ],
        },
        {
          kind: "ascii",
          label: "Moat map",
          column: "right",
          accent: true,
          lines: [
            "      [indexed repos]",
            "             |",
            "             v",
            "      [better graph]",
            "             |",
            "             v",
            "   [better routing + evals]",
            "             |",
            "             v",
            "      [better outcomes]",
            "             |",
            "             v",
            "      [more adoption]",
          ],
        },
      ],
    },
    {
      kicker: "Roadmap",
      title: "Execution follows a credible sequence",
      subtitle: "One hard loop first, then expansion",
      variant: "split",
      palette: "default",
      initialReveal: 1,
      sections: [
        {
          kind: "timeline",
          label: "Near-term plan",
          column: "left",
          items: [
            "Prove one full path: register -> index -> query -> scorecard -> intervention.",
            "Benchmark token savings and runtime gains on design-partner repos and live evals.",
            "Turn the graph engine plus CLI into a repeatable control plane for agentic dev teams.",
          ],
        },
        {
          kind: "terminal",
          label: "Execution posture",
          column: "right",
          lines: [
            { kind: "command", text: "ship --path prove-one-loop" },
            { kind: "info", text: "Stabilize the core backend and graph model." },
            { kind: "info", text: "Fix auth, tenant isolation, and API consistency." },
            { kind: "info", text: "Show a measurable intervention path end to end." },
          ],
        },
      ],
    },
    {
      kicker: "Ask",
      title: "Seed the control plane for agentic software teams",
      subtitle: "Fund the operating layer above the model",
      variant: "split",
      palette: "retro",
      initialReveal: 1,
      sections: [
        {
          kind: "bullets",
          label: "Use of funds",
          column: "left",
          reveal: "items",
          items: [
            "Productize the graph core, operator workflow, and eval proof points.",
            "Validate repeatable token savings, speed gains, quality lift, and variance reduction across diverse technical scenarios.",
            "Turn the thesis into the default operating system for future dev leads.",
          ],
        },
        {
          kind: "terminal",
          label: "Success condition",
          column: "right",
          lines: [
            { kind: "command", text: "seed -> design partners -> benchmark proof -> control plane" },
            { kind: "info", text: "Spend less." },
            { kind: "info", text: "Find the issue sooner." },
            { kind: "info", text: "Solve it faster." },
            { kind: "dim", text: "Repeat until this becomes the default operating model." },
          ],
        },
      ],
    },
    {
      kicker: "Close",
      title: "Aethyme",
      subtitle: "Less waste. Better diagnosis. Faster solves. More determinism.",
      variant: "interstitial",
      palette: "retro",
      initialReveal: 1,
      interstitialArt: [
        "      +-----------------------------------------------+",
        "      |  BUILD THE MANAGER FOR AGENTIC DEV TEAMS.     |",
        "      +-----------------------------------------------+",
      ],
      sections: [
        {
          kind: "lead",
          text:
            "The shift from human-only teams to human-led fleets of agents needs a new management layer.",
        },
        {
          kind: "lead",
          text: "Aethyme is that layer.",
        },
      ],
    },
  ],
};

module.exports = {
  aethymeInvestorDeck,
};
