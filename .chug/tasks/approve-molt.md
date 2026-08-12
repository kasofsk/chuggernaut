# Approve the molt

**You are the last gate, and you are here for judgement.** Every mechanical
objection has already been spent: `check-molt.sh` balanced the books,
`review-molt` judged the aim, `doc-lint` and `ci` passed, and
`review-molt-loss.md` fanned out readers instructed to refute. Nothing below asks
you to re-run any of that.

You are being asked one question that none of them can answer:

> **Will this project miss anything that just went away?**

A molt removes sentences that are *still true*. That is the point of it, so
"a fact is gone" is never the finding. The finding is that a fact worth keeping
is gone, and the only reliable detector is somebody who knows what this project
is about to do next.

## Start here

1. **The work agent's summary.** It is written for you specifically: what was shed
   by named class, the calls that were close and which way they went, what was
   promoted and where, and what was deliberately kept. Read the close calls first
   — a summary of only the easy deletions is hiding the reviewable part, and that
   itself is worth failing for.
2. **`.chug/molt-ledger`**, for the itemisation. Every shed line names the class
   that licensed it.
3. **The diff**, for anything the first two made you suspicious of.

## The one thing to actually hunt for

Design #533 names its own most likely defeat, and it is the thing a person is
uniquely able to catch: **a rejected alternative whose alternative is still
purchasable.** Someone argued once that we should not do X, the argument was
good, and the reasoning got shed because it reads like settled prose — old, names
a job, discusses a decision that is now simply how the system works.

No gate can see it, because the lost sentence names no path, no constant and no
link. No diff review can see it, because a deletion among legitimate deletions
has no signature. It stays invisible until someone proposes X again and there is
no argument to hand.

So when you see a deletion that argued *against* something, ask whether that
something is still available to do. If it is, the passage should have survived.

Two smaller things worth a glance, both cheap:

- **An open hole** — anything unclosed, unverified, unrun, or deferred with a
  cause — is immune regardless of age. If one was shed, that is a straight fail.
- **A promoted fact that never arrived.** Where a citation now points at a
  reference doc, open that doc and check the fact is really in it. A repointed
  citation aimed at a doc that never received the fact resolves cleanly and lies.

## Resolving

`POST /api/v1/projects/{owner}/{project}/jobs/{seq}/tasks/{task_id}/resolve`

- **Pass** — `{"kind":"Pass","structured":{"notes":"..."}}`, or `"structured":null` for
  no note. The key is not optional, and your note goes there rather than in
  `summary`, which is work-task-only and is discarded on an evaluator resolution
  (spec §1.2). The molt merges.
- **Fail** — `{"kind":"Fail","structured":{"findings":[{"file":"...","issue":"...","suggestion":"..."}]}}`.
  The work agent is re-invoked with your findings verbatim, so name the passage
  and say what its absence would cost. It has the ledger from its own previous
  attempt as input.
- **Escalate** — add `"abort": true` to a Fail when rework cannot fix it (the
  brief was wrong, or the scope was), which skips further rework.

**Take the time.** A molt is rare, it is large, and its damage is silent and
slow. Passing one you have not read is worse than passing an ordinary job you
have not read, because nothing downstream will ever catch what you missed.
