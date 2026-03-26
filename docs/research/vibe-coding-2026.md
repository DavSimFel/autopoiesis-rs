# Vibe Coding Research

State of the art as of March 26, 2026.

## Method Note

I attempted to use the MCP `fetch` tool as requested, but its transport repeatedly failed in this environment. I completed the research by reading the live URLs directly. All citations below point to real 2026 URLs retrieved during this run, and I have excluded 2025-dated sources from the citation list even when they are commonly referenced in current discussions.

## How To Read This

- `Empirical` means survey data, mining studies, experience reports, or research papers.
- `Opinion / practice` means engineer blog posts, interviews, or journalism quoting practitioners.
- `Inference` means a synthesis I am making from multiple 2026 sources, not a direct claim from a single source.

## Executive Summary

- `Inference` In March 2026, "vibe coding" no longer means only Karpathy's original joke-like "forget the code exists" posture. The serious version now looks more like natural-language-first development with agents, explicit constraints, and human verification, while Karpathy himself is publicly pushing the term `agentic engineering` for professional use. URLs: https://observer.com/2026/02/andrej-karpathy-new-term-ai-coding/ ; https://www.kristindarrow.com/insights/the-state-of-vibecoding-in-feb-2026 ; https://simonwillison.net/2026/Mar/14/pragmatic-summit/ ; https://arxiv.org/abs/2603.11073
- `Empirical` The dominant scale problem is verification debt, not raw code generation speed. Sonar's 2026 survey says AI already accounts for 42% of committed code, 96% of developers do not fully trust AI-generated code, and only 48% always check it before committing. URLs: https://www.sonarsource.com/company/press-releases/sonar-data-reveals-critical-verification-gap-in-ai-coding/ ; https://www.sonarsource.com/state-of-code-developer-survey-report.pdf
- `Empirical` The strongest recurring failure modes are skipped QA, uncritical trust, hidden logic/security errors, code complexity growth, and missing architectural constraints around auth, isolation, multi-tenancy, and infrastructure. URLs: https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf ; https://arxiv.org/abs/2603.15911 ; https://arxiv.org/abs/2603.15921 ; https://arxiv.org/abs/2603.11073
- `Inference` The best 2026 cleanup advice is not "rewrite the vibe-coded system from scratch." It is "add structure around it": contracts, scenario suites, templates, CI, threat models, audit trails, and explicit non-delegation zones. URLs: https://arxiv.org/abs/2603.15691 ; https://arxiv.org/abs/2603.11073 ; https://simonwillison.net/2026/Mar/14/pragmatic-summit/ ; https://arxiv.org/abs/2603.06365
- `Inference` There is a workable consensus on scope: vibe coding is appropriate for prototypes, bounded greenfield work, scaffolding, and low-blast-radius internal tools; it becomes dangerous for production systems with real money, real users, real compliance, or weak verification capacity. URLs: https://www.techradar.com/pro/even-ai-skeptic-linus-torvalds-is-getting-involved-in-vibe-coding-so-could-this-herald-a-new-dawn-for-linux-probably-not ; https://www.itpro.com/security/ncsc-warns-vibe-coding-poses-a-major-risk ; https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf ; https://arxiv.org/abs/2603.11073

## 1. What Vibe Coding Is, and How the Definition Evolved

- `Empirical` The clearest research definition in 2026 comes from the ICSE-SEIP grey-literature review: vibe coding is a practice where coders rely on AI code generation through intuition and trial-and-error "without necessarily understanding or rigorously reviewing" the code. URL: https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf
- `Empirical` Kristin Darrow's February 2026 synthesis gives the more operational definition now common in industry: humans specify intent and evaluate results while AI performs implementation, and the human role shifts toward systems, constraints, and evaluation criteria rather than typing code. URL: https://www.kristindarrow.com/insights/the-state-of-vibecoding-in-feb-2026
- `Opinion / practice` Andrej Karpathy's own public framing has already moved. Observer reports that he now distinguishes the original "vibe coding" era, mostly casual and fun, from `agentic engineering`, where engineers direct and oversee agents in professional settings and claim the leverage "without any compromise on the quality of the software." URL: https://observer.com/2026/02/andrej-karpathy-new-term-ai-coding/
- `Opinion / practice` A January 2026 report on Karpathy's posts says he shifted from roughly 80% manual coding in November 2025 to roughly 80% agent coding in December 2025, but also warned that if the code matters you must "watch them like a hawk," because the models behave like a rushed junior developer and overproduce abstractions. URL: https://devby.io/en/news/ai-writes-80-of-my-code-the-author-of-vibe-coding-changed-his-view-on-ai-agents-in-just-3-months
- `Opinion / practice` Simon Willison's March 2026 framing places vibe coding inside a wider adoption curve: the important transition is the moment when agents write more code than the human does, after which the engineering problem shifts from typing to supervision and quality control. URL: https://simonwillison.net/2026/Mar/14/pragmatic-summit/
- `Inference` The term has bifurcated. In popular use, it now covers most natural-language-first AI coding. In serious engineering discussions, it increasingly refers to the low-ceremony end of the spectrum, while production-grade use is being relabeled as a more structured successor with stronger review and testing requirements. URLs: https://observer.com/2026/02/andrej-karpathy-new-term-ai-coding/ ; https://www.kristindarrow.com/insights/the-state-of-vibecoding-in-feb-2026 ; https://arxiv.org/abs/2603.11073 ; https://simonwillison.net/2026/Mar/10/

## 2. Known Failure Modes: What Goes Wrong at Scale

- `Empirical` The biggest organizational failure mode is review capacity collapse. Sonar's 2026 survey says AI code volume is rising fast, but review/verification has become the bottleneck instead of disappearing. URLs: https://www.sonarsource.com/company/press-releases/sonar-data-reveals-critical-verification-gap-in-ai-coding/ ; https://www.sonarsource.com/state-of-code-developer-survey-report.pdf
- `Empirical` The ICSE-SEIP review found a strong speed-vs-QA tradeoff in practitioner behavior: 68% of code-quality perceptions were `fast but flawed`, 19% were `fragile or error-prone`, only 3% were `high quality and clean`, 36% of QA behavior was `skipped QA`, 18% was `uncritical trust`, 10% was `delegated QA to AI`, and 5% was `reprompting instead of debugging`. URL: https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf
- `Empirical` The same ICSE-SEIP review found that 11% of reported experiences ended in `code breakdown or abandonment`, typically when generated outputs became too complex, buggy, or inconsistent to fix. URL: https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf
- `Empirical` A large-scale March 2026 code-review study across 278,790 review conversations found that humans exchange 11.8% more rounds when reviewing AI-generated code than human-written code, that AI review suggestions are adopted at a significantly lower rate than human suggestions, and that adopted AI suggestions tend to increase code complexity and code size more than adopted human suggestions. URL: https://arxiv.org/abs/2603.15911
- `Empirical` A January 2026 study of AI-authored build-code pull requests found 364 maintainability and security-related build smells, including weak error handling and hardcoded paths/URLs, while also noting that more than 61% of agent-authored build PRs were approved and merged with minimal human intervention. URL: https://arxiv.org/abs/2601.16839
- `Empirical` VIBEPASS, submitted March 16, 2026, shows a subtler but important failure mode: frontier models are often good at producing syntactically valid tests, but poor at generating the discriminative tests and fault hypotheses actually needed to surface latent bugs; when self-generated tests do not really witness the fault, the repair can become worse than the unguided baseline. URL: https://arxiv.org/abs/2603.15921
- `Empirical` The small-team March 2026 experience report `Context Before Code` found that generated code often under-specifies isolation rules and infrastructure constraints when those are not explicitly stated, and that multi-tenancy, access control, memory policies, and asynchronous processing had to be deliberately designed and audited by humans. URL: https://arxiv.org/abs/2603.11073
- `Empirical` The February 2026 `GoodVibe` paper treats insecure-but-functional output as a normal failure mode of current code-generation models, arguing that functionally correct code still frequently arrives with security weaknesses when security requirements are not made explicit. URL: https://arxiv.org/abs/2602.10778
- `Opinion / practice` Richard Horne, chief executive of the UK's NCSC, warned on March 25, 2026 that vibe coding has obvious attractions but also creates major risk, and that current AI-generated code presents "intolerable risks" for many organizations because vulnerability-management maturity is not keeping up. URL: https://www.itpro.com/security/ncsc-warns-vibe-coding-poses-a-major-risk
- `Opinion / practice` Karpathy's January 2026 comments add a practitioner-level diagnosis: agents make hidden assumptions, fail to ask clarifying questions, overcomplicate solutions, and can trigger a `slopocalypse` of nearly-correct but low-quality artifacts if humans optimize only for speed. URL: https://devby.io/en/news/ai-writes-80-of-my-code-the-author-of-vibe-coding-changed-his-view-on-ai-agents-in-just-3-months

## 3. Emerging Best Practices for Cleaning Up Vibe-Coded Codebases

- `Empirical` Put context before code. The strongest March 2026 experience report says cleanup starts with explicit architectural constraints, project isolation rules, RBAC, memory policies, and processing boundaries before asking agents to change code. URL: https://arxiv.org/abs/2603.11073
- `Vision / research` Add contracts to every fuzzy prompt. `VibeContract` proposes decomposing high-level intent into explicit task sequences plus task-level contracts that define inputs, outputs, constraints, and behavioral properties, then using those contracts for testing, runtime verification, and debugging. URL: https://arxiv.org/abs/2603.15691
- `Opinion / practice` Seed the repository with good patterns so the agent copies them. Simon Willison argues that if the codebase already contains clean tests, CI, and established patterns, agents will follow those patterns "almost to a tee"; in other words, repo hygiene is now part of prompt engineering. URL: https://simonwillison.net/2026/Mar/14/pragmatic-summit/
- `Opinion / practice` Use scenario-based holdout validation, not just agent-authored unit tests. Simon's February 2026 write-up of StrongDM's `software factory` workflow highlights scenario sets stored outside the codebase as a way to validate behavior without letting the same agent both implement and grade its own work. URL: https://simonwillison.net/2026/Feb/7/software-factory/
- `Empirical` Add human-understandable decision records when promoting AI-authored changes. The ICSE-SEIP review's concrete practitioner recommendation is to use vibe coding for exploration and prototyping, but before production add tests, code review, and traceable records of why the AI change was accepted, which checks passed, and which risks were knowingly accepted. URL: https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf
- `Empirical` Keep humans in the review loop for contextual feedback, not just bug spotting. The March 2026 code-review study found that human reviewers provide understanding, testing, and knowledge-transfer feedback that AI reviewers still lack. URL: https://arxiv.org/abs/2603.15911
- `Empirical` Use independent security verification rather than trusting the generating model to self-correct. `ESAA-Security` turns auditing into a constrained, replayable pipeline with 26 tasks, 16 security domains, and 95 executable checks, explicitly separating agent reasoning from deterministic state changes and final reporting. URL: https://arxiv.org/abs/2603.06365
- `Empirical` Add LLM-aware dynamic testing. `SAFuzz` was built specifically because ordinary testing frameworks do not keep pace with the volume of AI-generated code; it combines behavioral diversification, harness generation, and adaptive resource allocation to find vulnerabilities more efficiently. URL: https://arxiv.org/abs/2602.11209
- `Inference` The common thread in 2026 best practice is that cleanup is becoming specification work: teams are spending less effort on hand-writing boilerplate and more effort on writing architecture, contracts, scenario suites, and verifiers that constrain what the agents may do. URLs: https://arxiv.org/abs/2603.11073 ; https://arxiv.org/abs/2603.15691 ; https://simonwillison.net/2026/Mar/14/pragmatic-summit/ ; https://www.sonarsource.com/state-of-code-developer-survey-report.pdf

## 4. Tools and Approaches for Auditing AI-Generated Code Quality

The clearest 2026 shift is that `AI coding` and `AI code auditing` are now separate product and research categories rather than the same thing. URLs: https://www.sonarsource.com/company/press-releases/sonar-data-reveals-critical-verification-gap-in-ai-coding/ ; https://www.axios.com/2026/03/06/openai-codex-security-ai-cyber ; https://www.axios.com/2026/02/23/cyber-stocks-anthropic-sell-off

| Tool / approach | Type | What it does | Why it matters |
| --- | --- | --- | --- |
| Codex Security | Official product | OpenAI says it finds, validates, and proposes fixes for vulnerabilities in repositories; Axios reports it pressure-tests suspected issues in sandboxed environments and proposes fixes, while OpenAI's help page says it builds a repo-specific threat model, validates exploitability, and surfaces minimal patches for human review. URLs: https://www.axios.com/2026/03/06/openai-codex-security-ai-cyber ; https://help.openai.com/en/articles/20001107-codex-security | This is the most explicit example of an agentic auditor that tries to verify exploitability instead of stopping at lint-like warnings. |
| Claude Code Security | Official product reported in 2026 coverage | Axios reports Anthropic's Claude Code Security can scan codebases for vulnerabilities and suggest patches. URL: https://www.axios.com/2026/02/23/cyber-stocks-anthropic-sell-off | Confirms that AI labs now treat security review as a standalone agent workflow, not just an IDE autocomplete feature. |
| ESAA-Security | Research architecture | Structures auditing as a governed pipeline with append-only events, constrained outputs, severity classification, risk matrices, and final markdown/JSON reports. URL: https://arxiv.org/abs/2603.06365 | Useful when the problem is auditability and reproducibility, not just detection. |
| SAFuzz | Research toolchain | Semantic-guided adaptive fuzzing for LLM-generated code; the paper reports precision rising from 77.9% to 85.7%, a 1.71x time-cost reduction, and complementary gains with unit-test generation. URL: https://arxiv.org/abs/2602.11209 | Important because it is built specifically for AI-generated-code volume and bug patterns. |
| GoodVibe | Research method | Security-by-default neuron-level tuning for code models; the paper reports up to a 2.5x security improvement over base models while preserving general utility. URL: https://arxiv.org/abs/2602.10778 | This attacks audit burden upstream by making generators less insecure before code review even starts. |
| VibeContract | Research / QA vision | Generates task contracts from natural-language intent and uses them for testing, runtime verification, debugging, and traceability. URL: https://arxiv.org/abs/2603.15691 | Strong candidate for teams that need maintainability and audit trails without abandoning agentic development. |
| Human-AI Synergy in Agentic Code Review | Research | Measures where AI reviewers help and where humans still outperform them in review conversations and suggestion quality. URL: https://arxiv.org/abs/2603.15911 | Best current evidence that AI review should augment, not replace, human review. |
| Testing with AI Agents | Research | Finds AI authored 16.4% of real-world test-adding commits and that AI-generated tests can yield coverage comparable to human-written tests. URL: https://arxiv.org/abs/2603.13724 | Shows that AI can be useful on the verifier side too, provided teams do not confuse coverage with full correctness. |

## 5. How Teams Transition from Vibe Coding to Maintainable Code Without Rewriting

- `Inference` No strong 2026 source I found recommends blanket rewrites as the default response to vibe-coded systems. The common recommendation is to wrap the existing system in stronger specifications, tests, and review controls, then refactor or replace only the parts that remain unverifiable. URLs: https://arxiv.org/abs/2603.15691 ; https://arxiv.org/abs/2603.11073 ; https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf ; https://simonwillison.net/2026/Mar/14/pragmatic-summit/

### Practical Migration Pattern

1. `Freeze the architecture surface.` Write down the real invariants first: trust boundaries, data ownership, multi-tenancy rules, auth, async behavior, and memory policies. The March 2026 experience report shows these are the places conversational generation most often underspecifies. URL: https://arxiv.org/abs/2603.11073
2. `Convert prompts into contracts.` Use explicit task-level contracts and maintain traceability between user intent, generated tasks, contracts, and code. URL: https://arxiv.org/abs/2603.15691
3. `Establish non-delegation zones.` Keep humans responsible for access control, project isolation, billing, security boundaries, compliance logic, and other places where a one-line prompt hides many latent invariants. URL: https://arxiv.org/abs/2603.11073
4. `Seed the repo with patterns agents should copy.` Put tests in the right place, keep CI green, and maintain small examples of the preferred style; Simon Willison's point is that agents mirror repo quality just as human teammates do. URL: https://simonwillison.net/2026/Mar/14/pragmatic-summit/
5. `Introduce independent verification for every AI-authored change.` That means tests, human review, threat models, fuzzing, and security review, rather than letting the generating agent be the sole judge of correctness. URLs: https://www.sonarsource.com/state-of-code-developer-survey-report.pdf ; https://arxiv.org/abs/2603.15911 ; https://arxiv.org/abs/2603.06365 ; https://arxiv.org/abs/2602.11209
6. `Use agents for targeted cleanup, not open-ended reinvention.` The build-code-quality study explicitly found that agent-authored changes can remove existing smells through refactorings as well as introduce them, which supports incremental repair over full rewrites. URL: https://arxiv.org/abs/2601.16839
7. `Keep an audit trail.` The emerging pattern across ICSE-SEIP and ESAA-Security is to record why a change was accepted, what validated it, and what risks remain, so the codebase does not become an archaeology problem six months later. URLs: https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf ; https://arxiv.org/abs/2603.06365

## 6. Latest Opinions from Respected Engineers and Technical Leaders

- `Andrej Karpathy, January-February 2026` Karpathy's own public posture has changed from celebrating vibe coding as a fun low-friction workflow to describing a more professional `agentic engineering` model. At the same time, he says agents are now doing most of his coding, but that important code still requires hawk-like supervision. URLs: https://devby.io/en/news/ai-writes-80-of-my-code-the-author-of-vibe-coding-changed-his-view-on-ai-agents-in-just-3-months ; https://observer.com/2026/02/andrej-karpathy-new-term-ai-coding/
- `Simon Willison, March 2026` Willison's March view is the clearest quality-oriented pro-agent stance: worse code is not an inevitable result of using agents; it is a process choice. His recurring position is that AI should help teams produce better code, not merely more code, and that clean templates, tests, and review discipline matter more in an agentic workflow, not less. URLs: https://simonwillison.net/2026/Mar/10/ ; https://simonwillison.net/2026/Mar/14/pragmatic-summit/
- `Linus Torvalds, January 2026` Torvalds's view is pragmatic and narrow: AI-assisted or vibe-coded work is acceptable for learning assistance and unimportant hobby projects, but he does not treat that as an endorsement for trusted systems like Linux, Git, or other production infrastructure. URL: https://www.techradar.com/pro/even-ai-skeptic-linus-torvalds-is-getting-involved-in-vibe-coding-so-could-this-herald-a-new-dawn-for-linux-probably-not
- `Richard Horne / NCSC, March 2026` Horne's stance is the strongest security warning in the March 2026 mainstream coverage: the upside is real, but tools must be secure-by-default and the current risk profile is unacceptable for many organizations without stronger controls. URL: https://www.itpro.com/security/ncsc-warns-vibe-coding-poses-a-major-risk

## 7. Is There a Consensus on When Vibe Coding Is Appropriate vs Dangerous?

### Broad Consensus Areas

- `Inference` Appropriate use cases in 2026 are prototypes, experiments, one-off internal tools, bounded greenfield scaffolding, and "unfamiliar but non-core" components where a human can still review the output and the blast radius is low. URLs: https://www.techradar.com/pro/even-ai-skeptic-linus-torvalds-is-getting-involved-in-vibe-coding-so-could-this-herald-a-new-dawn-for-linux-probably-not ; https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf ; https://simonwillison.net/2026/Mar/14/pragmatic-summit/
- `Inference` Dangerous use cases are production systems with payments, auth, compliance, multi-tenancy, public attack surface, weak test coverage, or teams already short on reviewers, because those are exactly the contexts where hidden invariants, verification debt, and security bugs become expensive. URLs: https://www.itpro.com/security/ncsc-warns-vibe-coding-poses-a-major-risk ; https://arxiv.org/abs/2603.11073 ; https://www.sonarsource.com/state-of-code-developer-survey-report.pdf ; https://arxiv.org/abs/2603.15911
- `Inference` The emerging consensus is therefore not "never use vibe coding" and not "AI can safely replace engineering." It is "use agents aggressively where verification is cheap, and add more engineering discipline as the cost of being wrong rises." URLs: https://observer.com/2026/02/andrej-karpathy-new-term-ai-coding/ ; https://simonwillison.net/2026/Mar/10/ ; https://www.techradar.com/pro/even-ai-skeptic-linus-torvalds-is-getting-involved-in-vibe-coding-so-could-this-herald-a-new-dawn-for-linux-probably-not ; https://www.itpro.com/security/ncsc-warns-vibe-coding-poses-a-major-risk

### Simple Decision Rule

- `Safe enough to vibe` if the code is disposable or easily replaceable, the architecture is simple, the team can understand the output, and verification is fast. URLs: https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf ; https://simonwillison.net/2026/Mar/14/pragmatic-summit/
- `Dangerous to vibe` if the same prompt is standing in for architecture, security policy, operational constraints, and QA discipline at once. URLs: https://arxiv.org/abs/2603.11073 ; https://arxiv.org/abs/2603.15691 ; https://www.itpro.com/security/ncsc-warns-vibe-coding-poses-a-major-risk

## Bottom Line

- `Inference` March 2026 does not show a collapse of vibe coding. It shows a split. The low-discipline version remains great for prototypes and fast learning, but teams trying to scale it are rediscovering software engineering the hard way: contracts, tests, architecture, code review, threat models, and audit trails did not go away. URLs: https://observer.com/2026/02/andrej-karpathy-new-term-ai-coding/ ; https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf ; https://www.sonarsource.com/state-of-code-developer-survey-report.pdf ; https://arxiv.org/abs/2603.11073
- `Inference` The real "state of the art" in March 2026 is not better prompt phrasing. It is the emergence of verification systems around AI-generated code: contract-driven QA, scenario holdouts, threat-model-driven auditors, AI-aware fuzzing, and tighter human review loops. URLs: https://arxiv.org/abs/2603.15691 ; https://simonwillison.net/2026/Feb/7/software-factory/ ; https://help.openai.com/en/articles/20001107-codex-security ; https://arxiv.org/abs/2602.11209 ; https://arxiv.org/abs/2603.15911

## Source List Used

| Date | Source type | URL |
| --- | --- | --- |
| 2026-01-08 | Vendor survey / press release | https://www.sonarsource.com/company/press-releases/sonar-data-reveals-critical-verification-gap-in-ai-coding/ |
| 2026-01-14 | Journalism quoting Linus Torvalds | https://www.techradar.com/pro/even-ai-skeptic-linus-torvalds-is-getting-involved-in-vibe-coding-so-could-this-herald-a-new-dawn-for-linux-probably-not |
| 2026-01-28 | Journalism summarizing Karpathy posts | https://devby.io/en/news/ai-writes-80-of-my-code-the-author-of-vibe-coding-changed-his-view-on-ai-agents-in-just-3-months |
| 2026-02-07 | Engineer blog / commentary | https://simonwillison.net/2026/Feb/7/software-factory/ |
| 2026-02-09 | Journalism quoting Karpathy | https://observer.com/2026/02/andrej-karpathy-new-term-ai-coding/ |
| 2026-02-11 | Research paper | https://arxiv.org/abs/2602.10778 |
| 2026-02-11 | Research paper | https://arxiv.org/abs/2602.11209 |
| 2026-02-14 | Industry synthesis | https://www.kristindarrow.com/insights/the-state-of-vibecoding-in-feb-2026 |
| 2026-02-23 | Journalism on Claude Code Security | https://www.axios.com/2026/02/23/cyber-stocks-anthropic-sell-off |
| 2026-03-06 | Journalism on Codex Security | https://www.axios.com/2026/03/06/openai-codex-security-ai-cyber |
| 2026-03-10 | Engineer blog / commentary | https://simonwillison.net/2026/Mar/10/ |
| 2026-03-10 | Experience report | https://arxiv.org/abs/2603.11073 |
| 2026-03-14 | Engineer blog / commentary | https://simonwillison.net/2026/Mar/14/pragmatic-summit/ |
| 2026-03-14 | Empirical paper | https://arxiv.org/abs/2603.13724 |
| 2026-03-16 | Empirical paper | https://arxiv.org/abs/2603.15911 |
| 2026-03-16 | Empirical / benchmark paper | https://arxiv.org/abs/2603.15921 |
| 2026-03-16 | QA vision paper | https://arxiv.org/abs/2603.15691 |
| 2026-03-25 | Journalism quoting NCSC | https://www.itpro.com/security/ncsc-warns-vibe-coding-poses-a-major-risk |
| 2026 (report) | Vendor survey PDF | https://www.sonarsource.com/state-of-code-developer-survey-report.pdf |
| 2026 (ICSE-SEIP) | Peer-reviewed paper | https://kblincoe.github.io/publications/2026_ICSE_SEIP_vibe-coding.pdf |
| 2026 (OpenAI help, updated March 2026 as retrieved) | Official product documentation | https://help.openai.com/en/articles/20001107-codex-security |
| 2026-03-06 | Audit architecture paper | https://arxiv.org/abs/2603.06365 |
| 2026-01-23 | Empirical paper | https://arxiv.org/abs/2601.16839 |
