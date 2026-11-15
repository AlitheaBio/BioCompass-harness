---
description: "Official BioCompass sidebar branding for official client builds."
kind: "package-reference"
---

# @deepseek-ai/dsh-client-ui-brand-official

## Summary

This package gives an `official` client build the BioCompass and Alithea bio sidebar artwork. The shipped Web and Desktop compositions load it by default. Non-official build profiles keep their local sidebar fallback labels and leave the hero on its default animated BioCompass mark. This plugin adds browser presentation only and issues no model requests.

## Table of Contents

- [Use this package](#use-this-package)
- [Understand the implementation](#understand-the-implementation)
- [Further Exploration](#further-exploration)
- [Model Experience](#model-experience)
- [Known Limitations and Deferred Work](#known-limitations-and-deferred-work)
- [Dev Note](#dev-note)

-----

<a id="use-this-package"></a>
## Use this package

The shipped Web and Desktop compositions already mount this plugin. When `DSH_CLIENT_BUILD_PROFILE` is `official`, it registers occupants for the sidebar brand mark and sidebar brand wordmark slots.

### Choosing the profile

`DSH_CLIENT_BUILD_PROFILE` selects which sidebar brand renders. An `official` build registers both `sidebar.brand.mark` and `sidebar.brand.name` using the BioCompass Halo-Orb mark and BioCompass + Alithea bio wordmark from `dsh-client-ui-primitives`; any other value leaves the shell fallbacks in place. The conversation hero displays the BioCompass mark from `dsh-client-ui-conversation` across all profiles. The plugin still loads and validates in non-official builds because only slot registration checks the build profile.

### Replacing the brand

A deployment with its own identity leaves this package out and composes another package that occupies `sidebar.brand.mark`, `sidebar.brand.name`, and optionally `conversation.hero.brand.mark`. Occupying a slot is the sole composition route; the artwork values take no runtime configuration.

-----

<a id="understand-the-implementation"></a>
## Understand the implementation

<details>
<summary>Implementation internals — click to expand</summary>

`apply` waits on each sidebar slot declaration via `ctx.slots.inject('sidebar.brand.mark', ...)` and `ctx.slots.inject('sidebar.brand.name', ...)`, returning early when `DSH_CLIENT_BUILD_PROFILE !== 'official'`. Both occupants render primitives from `@deepseek-ai/dsh-client-ui-primitives` (`FishLogo` for the orb mark and `BrandWordmark` with `includeMark={false}` for the paired wordmark). The browser entry point is [`src/client/index.ts`](src/client/index.ts); the Node entry point is an empty Loader seat. The browser document title stays outside the slot system under `DSH_CLIENT_TITLE`.

</details>

-----

<a id="further-exploration"></a>
## Further Exploration

Read these pages when the brand surface is not enough. They move from the slots this package occupies to the shell that renders them.

- [ui-sidebar](../ui-sidebar/README.md) — declares `sidebar.brand.mark` and `sidebar.brand.name` and renders their fallbacks.
- [Web client architecture](../../../docs/subsystems/web-client.md) — how browser plugin rows load and register slots.

-----

<a id="model-experience"></a>
## Model Experience

None, as the package contributes browser presentation only; nothing here reaches a model request.

#### KV Cache effect

None; this package neither assembles nor sends a provider request.

## Known Limitations and Deferred Work

<a id="known-limitations-and-deferred-work"></a>

These limits define how brand presentation is supplied. They are current package constraints, not a brand-design comparison or a task backlog.

- **One occupant set** — alternative presentation belongs in another Cordis package occupying the same slots.
- **The browser title is independent** — `DSH_CLIENT_TITLE` selects title text at build time rather than through a UI slot.

<a id="dev-note"></a>
### Dev Note

<details>
<summary>Working context for maintainers — click to expand</summary>

None.

</details>
