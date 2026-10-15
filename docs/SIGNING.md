# Signing Joseon for Windows

The release workflow can ask [SignPath](https://signpath.org/) to Authenticode-sign `Joseon.exe`. It does nothing until the project is accepted into SignPath's free open-source program and the GitHub secret and variables below are all set. Until then a tag still publishes the unsigned zip, as it does today.

Joseon is GPL-3.0-only. The certificate, once issued, is in the name of SignPath Foundation, not in the name of this project.

## Turn signing on

1. Apply at <https://signpath.org/apply.html>. The conditions are on <https://signpath.org/terms.html>.
2. In SignPath, add the predefined trusted build system **GitHub.com** to the organization and link it to the project. Install the SignPath GitHub App on `jasas1/JoseonPC`. The connector checks that a GitHub-hosted workflow built the file. See [GitHub trusted build systems](https://docs.signpath.io/trusted-build-systems/github).
3. Create a project whose repository URL is `https://github.com/jasas1/JoseonPC`. Note the organization id and the project slug. Project fields are described in [Setting up projects](https://docs.signpath.io/projects).
4. Create a signing policy and note its slug. For this program it needs a manual approval, trusted-build-system verification, and origin verification. The submitter is the CI user that owns the API token. With trusted-build-system verification, an interactive user cannot be the submitter ([Signing policies](https://docs.signpath.io/projects)).
5. Set that project's **default** artifact configuration to the XML below. The workflow does not pass an artifact-configuration slug, so SignPath uses the default ([action inputs](https://docs.signpath.io/trusted-build-systems/github)).
6. On the GitHub repository `jasas1/JoseonPC`, set:

   | Kind | Name |
   |---|---|
   | Secret | `SIGNPATH_API_TOKEN` |
   | Variable | `SIGNPATH_ORGANIZATION_ID` |
   | Variable | `SIGNPATH_PROJECT_SLUG` |
   | Variable | `SIGNPATH_SIGNING_POLICY_SLUG` |

   Do not invent these. Copy them from the SignPath project after it exists. The token needs permission to submit a signing request for that policy.

If any one of the four is missing or blank, the release job logs that signing is off and publishes the unsigned build. If all four are set, a failure at SignPath fails the job. The unsigned exe is not published as a fallback.

## What the workflow does when signing is on

The job is `.github/workflows/release.yml`. It builds on `windows-latest` from the tagged source (GitHub-hosted runners are an open-source requirement). It then:

1. Compiles with `JOSEON_REQUIRE_SIGNATURE=1`, which writes a `VERSIONINFO` resource (`ProductName` Joseon, `OriginalFilename` Joseon.exe, product and file version equal to the version in `Cargo.toml`).
2. Smokes the unsigned `Joseon.exe`.
3. Uploads that exe with `actions/upload-artifact@v4`. That action always stores a zip. This is the artifact SignPath reads.
4. Submits it with [`signpath/github-action-submit-signing-request@v3`](https://github.com/signpath/github-action-submit-signing-request) and waits up to 40 minutes (`wait-for-completion-timeout-in-seconds: 2400`). The job timeout is 90 minutes. Someone must approve the request in SignPath inside that wait. Open-source signing is manual on purpose ([terms](https://signpath.org/terms.html)).
5. Replaces `dist/.../Joseon.exe` with the signed file and smokes it again.
6. Zips the folder and publishes the GitHub release. Only `Joseon.exe` is signed. `joseon-probe.exe` is a test tool and stays unsigned.

The action parameter `version` is the `Cargo.toml` version, for example `0.0.11`, sent as a JSON string. The artifact configuration must declare that parameter. An extra parameter that the configuration does not declare will fail the request.

`actions/upload-artifact@v4` has no `archive: false` input, so the configuration root is a zip. The signing action unpacks the signed zip (its `skip-decompress` input stays at the default `false`).

## Artifact configuration

Paste this as the project's default artifact configuration. The syntax is the [artifact configuration](https://docs.signpath.io/artifact-configuration/) language; metadata attributes are listed in the [reference](https://docs.signpath.io/artifact-configuration/reference), and a zip of several files is shown in the [examples](https://docs.signpath.io/artifact-configuration/examples).

```xml
<?xml version="1.0" encoding="utf-8" ?>
<artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
  <parameters>
    <parameter name="version" required="true" />
  </parameters>
  <zip-file>
    <pe-file path="Joseon.exe"
             product-name="Joseon"
             product-version="${version}"
             file-version="${version}"
             original-filename="Joseon.exe">
      <authenticode-sign />
    </pe-file>
  </zip-file>
</artifact-configuration>
```

Open-source subscriptions require those product-name and product-version restrictions, and the same version on every binary of a build. The resource compiler runs only when `JOSEON_REQUIRE_SIGNATURE=1` (`crates/joseon-app/build.rs`). A normal build does not need `rc.exe`. If the signing build cannot write the resource, the build fails and nothing is published.

## What the updater does with the signature

`JOSEON_REQUIRE_SIGNATURE=1` is read at compile time (`option_env` in `crates/joseon-app/src/update.rs`). A program built that way calls `WinVerifyTrust` on the unpacked `Joseon.exe` before it replaces the running one. The check asks for a valid Authenticode signature and checks the chain for revocation. There is no dialog. If the check fails, the old program stays.

A program built without that flag does not check a signature. Current releases are unsigned, so the flag stays off until the four values above are set. The first signed release can still replace those older builds. After a signed build is installed, later releases have to be signed too: removing the secret or the variables makes the next zip unsigned, and that installed build will refuse it.

## Open-source requirements

Summarized from <https://signpath.org/terms.html>. Read that page before applying. It is the list SignPath enforces; this file does not replace it.

- OSI-approved license, no commercial dual license, no proprietary code in the signed program. System libraries may be included. The project must be maintained, already released, and described on its download page.
- The certificate is issued to SignPath Foundation. Sign only this project's own binaries, built from this repository. Do not sign another project's files with this subscription.
- Every release is approved by a person. Builds come from source on GitHub-hosted runners, which this workflow already does.
- Multi-factor authentication on SignPath and on GitHub.
- Name who may commit, who reviews changes, and who may approve a signing request.
- The project home page and the download or release pages must contain a section or a link titled "Code signing policy". That text must include "Free code signing provided by SignPath.io, certificate by SignPath Foundation", the team roles, and a privacy statement. The terms page gives the privacy sentence that fits a program that does not send data unless the user asks. Add that section when the project is accepted. It is not on the home page yet, because signing is not active.
- Product name and product version metadata must be set and enforced. The configuration above does that.

## Sources

- Application: <https://signpath.org/apply.html>
- Program description: <https://signpath.org/>
- Open-source conditions: <https://signpath.org/terms.html>
- GitHub trusted build system and the submit-signing-request action: <https://docs.signpath.io/trusted-build-systems/github>
- Action source: <https://github.com/signpath/github-action-submit-signing-request>
- Projects and signing policies: <https://docs.signpath.io/projects>
- Signing from a build: <https://docs.signpath.io/signing-code>
- Artifact configuration: <https://docs.signpath.io/artifact-configuration/>, <https://docs.signpath.io/artifact-configuration/examples>, <https://docs.signpath.io/artifact-configuration/reference>
