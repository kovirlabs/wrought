# Governance

Wrought is a community project under the Kovir Labs umbrella. Its code is
MIT OR Apache-2.0 and needs no cloud service or vendor; this file says who
decides what, now and later.

## Now: a benevolent dictator

While the project is young, one person, the **project lead** (the founder),
has the final say on:

- what is merged and released, and when;
- the direction of the specification and the roadmap;
- who gets commit or release rights;
- the security process ([SECURITY.md](SECURITY.md)) and the release signing
  key.

The lead works in the open: design choices are recorded in
[DECISIONS.md](DECISIONS.md) with the alternatives that were rejected, the
roadmap lives in the [specification](wrought/), and anyone may propose a
change to either through an issue. A decision the lead makes against a
public objection is explained in the issue or in DECISIONS.md.

## Later: a small maintainer team

The project moves to a maintainer team once there are regular contributors.
The trigger is concrete: **at least two people other than the lead have
each had ten or more non-trivial pull requests merged over at least six
months**, and are willing to review others' work.

The transition:

1. The lead proposes the first maintainers in a public issue (usually
   people who already review and triage). Each must accept.
2. After two weeks for comments, the lead updates this file: the names
   under "Maintainers", the rules below, and the date.
3. The lead remains a maintainer and keeps a tie-breaking vote for the first
   year, after which the team decides whether to keep or drop it.

How the team will work:

- **Everyday changes:** one maintainer's approval (not the author's) is
  enough to merge.
- **Significant changes** (a new feature crate, a breaking change, a new
  dependency with a large tree, a change to this file or the security
  process): proposed in an issue, open for at least one week, merged by
  lazy consensus; if a maintainer objects and talking does not resolve it,
  a simple majority of maintainers decides.
- **Maintainers per area:** a maintainer may own a crate or area (labels in
  [CONTRIBUTING.md](CONTRIBUTING.md)) and is then its first reviewer.
- **New maintainers:** nominated by any maintainer, accepted without an
  objection from another maintainer within two weeks.
- **Stepping down:** anyone can step down at any time. A maintainer
  inactive for a year is moved to "Emeritus" after being asked.
- **Release rights and the signing key** are held by at least two
  maintainers, so a release never depends on one person.

## Maintainers

- Project lead: the founder (see the repository owner on GitHub).

## Conduct

Everyone in the project's spaces is expected to be respectful and
constructive. The lead (later: any maintainer) may remove comments, lock
threads or ban accounts that harass, demean or spam others; serious cases
are handled privately with the people involved.

## Changing this document

Changes to this file follow the "significant changes" rule above; until the
maintainer team exists, the lead decides after a public issue.
