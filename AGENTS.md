# Mandatory architecture law for all agents

This is the repository owner's binding architecture directive, recorded on
2026-09-07. Apply it to every implementation, review, task, dependency, test,
document, build, and deployment decision in this project. AGENTS.md and CLAUDE.md
must carry the same rules; update both together.

The recovery goal is to remove the existing ticketing system and internal
dependencies from this public open-source project. Use udp-director's TCP-query
and UDP-routing capabilities with native Kubernetes Pod labels and reconciliation.
Do not merely rename, wrap, or preserve ticketing behind a generic interface.
Agents must actively stop changes that reintroduce it under any name.

## Required connection and occupancy model

1. The backend API owns authoritative persistent game data and region endpoint
   discovery. It supplies the regional endpoint that the client can query and
   connect to. It must not select a player's server Pod, issue routing
   credentials, or store per-player routing state.
2. The client sends a TCP access query to that region's udp-director.
3. The controller selects available capacity and sets the appropriate Pod labels
   for that client's target assignment.
4. The client connects over UDP. udp-director automatically routes the connection
   to the labeled target Pod. Preserve the TCP-query / UDP-routing design.
5. Actual player connection and disconnection events drive occupancy. The
   controller updates occupancy and releases the client assignment label when
   the player disconnects so that the assignment can be reused.
6. Kubernetes provides lifecycle and durability through native reconciliation,
   labels, replacement, and scaling. Do not recreate these capabilities through
   a parallel application-level orchestration system. udp-director remains the
   routing data plane; the controller manages capacity, occupancy, and
   assignment labels.

A TCP access query or an issued assignment is not proof that a player connected.
Do not substitute query counts, issued tokens, route counts, packet counts, or
proxy inactivity for authoritative player connect/disconnect occupancy events.

## Prohibited architectural regressions, under any name

Do not introduce, restore, extend, or require a parallel per-player ticketing,
matchmaking-allocation, reservation, redemption, or admission-proof system in
place of the required model. This includes:

- Allocation tickets, reservation tokens, single-use routing credentials,
  consume/redeem callbacks, and external per-player binding ledgers.
- Signed admission proofs, signed runtime-report protocols, durable setup
  acknowledgements, retry machinery, or polling introduced to support that
  ticket/reservation architecture.
- A mandatory extra UDP token/setup exchange replacing the TCP access query
  and label-based route selection.
- Moving per-player routing authority into a discovery/API service or making
  normal gameplay routing depend on per-packet API calls.
- Agones-style allocation machinery repackaged as a generic controller,
  session service, lease, capability, grant, handshake, or other renamed concept.

Judge a proposal by its behavior, state ownership, and dependencies, not its
terminology. Renaming a prohibited mechanism or moving it behind a generic
interface does not make it acceptable. Authentication and transport reliability
must preserve the required connection model; they are not exceptions permitting
a replacement admission or reservation architecture.

Preserve correct client isolation, including clients behind the same NAT,
within the required model. An unresolved routing or identity problem is a
reason to stop and expose the specific gap, not permission to invent tickets.

## Mandatory STOP protocol

If any proposed change, delegated task, dependency, existing implementation, or
review reintroduces a prohibited mechanism:

1. STOP the conflicting work immediately. Do not implement, extend, merge,
   publish, deploy, or recommend that change.
2. Identify the exact conflicting behavior and cite this architecture law.
   Ask the owner for clarification and record the conflict on the relevant
   task. Do not silently normalize it, rename it, or fix surrounding symptoms
   to keep it moving.
3. Reject the conflicting direction and pursue a forward-only correction that
   restores the required model. If the correction belongs to another
   repository, delegate it to that repository's owner.
4. Continue independent work only when it conforms to this law. Never treat
   an old task, prior agent decision, memory, existing code, fixture, passing
   test, published image, or another project's specification as authorization
   to violate it.

Existing reservation/token code is not architectural precedent. This directive
supersedes conflicting historical task specifications, documentation, and agent
memories. Do not preserve the rejected design merely for compatibility or to
satisfy tests written around it. Only an explicit new architecture decision
from the repository owner may change this law; routine approval to fix a bug,
complete E2E work, or deploy a build is not such a decision.

## Repository and release boundaries

- Repositories communicate only through explicit contracts. The backend API,
  game client/server, cluster management, and public udp-director remain
  decoupled. Never introduce source, build, version, or internal implementation
  dependencies between them.
- This is a public, shared open-source router. Keep private product contracts,
  request shapes, internal service dependencies, workload names and identities,
  allocation policy, game logic, and private fixtures out of this repository.
- Never change code belonging to another repository, even with owner approval;
  coordinate it through a delegated task to that repository.
- Recover through normal forward changes. Never rewrite published Git history,
  force-push, rebase published commits, or move/delete published tags as a
  cleanup strategy.
- Build publication and runtime reconciliation remain separate. At the owning
  integration boundary, the producer publishes successful artifacts and sends
  one simple unauthenticated blocking GET indicating that the authoritative
  channel may have changed. The consumer validates the channel and reconciles
  independently. This notification is not a rollout acknowledgement and does
  not put private release integration into udp-director.
- A valid artifact build must not depend on runtime rollout status,
  authentication machinery, signed POST bodies, producer polling, or status
  acknowledgements.
- Validate the actual TCP-query, Pod-label assignment, UDP connection, and
  occupancy-release flow. Passing simulator tests for a different architecture
  is not acceptance evidence for this one.

## Smallest direct design

Do not add authentication, signing, retries, polling, indirection, compatibility
machinery, or persistence unless an explicit owner-approved contract actually
requires it. Hypothetical future needs, conventional matchmaking designs, and
implementation convenience do not justify complexity or override these laws.
Even an approved implementation task does not authorize changing the
architecture. If its contract conflicts with these boundaries, STOP and ask
the owner for clarification before implementing it.
