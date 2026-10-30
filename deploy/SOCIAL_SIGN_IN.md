# Google, GitHub and Microsoft user sign-in

These registrations let your application's users sign in. They do not configure the operator's ChatGPT accounts or API access. Email-code sign-in continues to work independently. Each social button appears in the client only when that provider's two credentials are configured on the backend.

All three providers require you, as the app operator, to register an application once. Users then only choose their account and consent. No provider credentials belong in the client executable, public website or Git repository.

## Google

1. Open [Google Cloud Console](https://console.cloud.google.com/) and sign in with the Google account that should own this application's registration.
2. In the project selector at the top, choose **New project**. Name it **Slax AI** and create it, then select it.
3. Open **Google Auth Platform** from the console search. If this is its first use, choose **Get started**. Under **Branding**, set the app name to **Slax AI**, select your user-support email, and supply your developer contact email. Save the initial configuration. If prompted for an audience, choose **External** to allow users outside your Workspace organization.
4. Under **Audience**, keep the registration in testing while you configure it and add your own Google email under **Test users**. Before allowing general users, use **Publish app** to change the audience to production. This is the same registration and credentials, not a second setup. Follow any branding/domain-verification requests Google shows; an optional custom logo can add review requirements.
5. Under **Data Access**, use only `openid`, `email`, and `profile`. These are basic sign-in scopes; no Gmail or Drive permissions are needed.
6. Under **Clients**, choose **Create client**, application type **Web application**, name **Slax AI backend**.
7. Under **Authorized redirect URIs**, add exactly:

   ```text
   https://ai.slax.org/auth/google/callback
   ```

   No localhost JavaScript origin is needed: the backend handles the authorization-code exchange.
8. Create the client and privately save its **Client ID** and **Client secret**. They become `GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_SECRET` in the server configuration below. If the secret is shown only once, save it before closing the dialog.

Google Gmail and verified Workspace addresses can sign in directly. Other Google-account email addresses may require our email code once to establish current ownership. Later sign-ins use the linked Google identity.

Official reference: [Google OpenID Connect setup](https://developers.google.com/identity/openid-connect/openid-connect).

## GitHub

1. Open [GitHub's new OAuth app form](https://github.com/settings/applications/new) while signed in as the intended owner. The navigation path is **Settings → Developer settings → OAuth Apps → New OAuth App**. Choose an **OAuth App**, not a GitHub App.
2. Fill in:

   | Field | Value |
   | --- | --- |
   | Application name | `Slax AI` |
   | Homepage URL | `https://ai.slax.org` |
   | Application description | `Sign in to Slax AI` |
   | Authorization callback URL | `https://ai.slax.org/auth/github/callback` |

3. Leave **Enable Device Flow** off if it is offered; this integration uses browser authorization with PKCE.
4. Click **Register application**. Copy the **Client ID**, then click **Generate a new client secret** and privately copy the secret. These become `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET`.

The app requests only `user:email`, never repository access. A verified GitHub email links to the same application account as email-code sign-in. If no verified email is available, the client asks you to verify one once.

Official reference: [Creating a GitHub OAuth app](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/creating-an-oauth-app).

## Microsoft

Microsoft has the most setup overhead: you need an accessible Microsoft Entra tenant and permission to register apps. If the portal says you have no directory, complete its tenant/Azure-account prerequisites first. You do not need to buy a premium Entra plan just for this code integration; follow the current portal's account requirements.

1. Sign in to [Microsoft Entra admin center](https://entra.microsoft.com/) using the account that should own this registration.
2. Open **Entra ID → App registrations → New registration**. The portal search can also find **App registrations**.
3. Enter **Slax AI** as the name.
4. For **Supported account types**, select **Accounts in any organizational directory and personal Microsoft accounts**. This allows both work/school accounts and personal Outlook/Hotmail accounts. Do not choose the single-tenant option.
5. For **Redirect URI**, select the **Web** platform and enter exactly:

   ```text
   https://ai.slax.org/auth/microsoft/callback
   ```

6. Click **Register**. On the **Overview** page, privately copy **Application (client) ID** as `MICROSOFT_CLIENT_ID`. This is not the directory/tenant ID.
7. Open **Certificates & secrets → Client secrets → New client secret**. Add a description, choose an expiry you can maintain, and click **Add**.
8. Privately copy the secret's **Value**, not its **Secret ID**, into `MICROSOFT_CLIENT_SECRET`. The value is only visible immediately after creation. Record its expiry so you can create a replacement and update the server before it expires.
9. Under **Authentication**, keep the callback on the **Web** platform. No implicit-grant switches or public-client flow are needed. The app requests only `openid`, `profile`, and `email`; it does not need mail or file access. Some organizations restrict third-party sign-in and can require their administrator's consent.

For a new Microsoft identity, the client asks for our email code once before linking it to an application account. Microsoft does not guarantee that its email claim proves email ownership. Once linked, future Microsoft sign-ins do not require that code.

Official references: [Registering a Microsoft identity application](https://learn.microsoft.com/en-us/entra/identity-platform/quickstart-register-app), [UserInfo endpoint](https://learn.microsoft.com/en-us/entra/identity-platform/userinfo).

## Configure your backend

Build and deploy the updated backend and client first. Keep the existing `ASSISTANT_PUBLIC_URL=https://ai.slax.org` and SMTP configuration. Apache's existing proxy to the backend also forwards `/auth/...`; no new virtual host is needed.

Run these commands on the server as root. Add the credentials from the provider portals to the existing private environment file; do not replace the other settings. Configure any subset of the providers initially.

```sh
# Edit the existing backend configuration; secrets stay on the server.
nano /etc/linux-assistant/server.env
```

Add the corresponding pairs, replacing these placeholders privately:

```ini
GOOGLE_CLIENT_ID=your-google-client-id
GOOGLE_CLIENT_SECRET=your-google-client-secret
GITHUB_CLIENT_ID=your-github-client-id
GITHUB_CLIENT_SECRET=your-github-client-secret
MICROSOFT_CLIENT_ID=your-microsoft-application-client-id
MICROSOFT_CLIENT_SECRET=your-microsoft-client-secret-value
```

```sh
# systemd reads this file as root; prevent other local users from reading its secrets.
chmod 600 /etc/linux-assistant/server.env

# Load the new binary and credentials. No conversation or account reset is needed.
systemctl restart linux-assistant.service

# This endpoint lists provider names only; it never returns credentials.
curl --fail --silent --show-error https://ai.slax.org/v1/auth/providers
```

With all providers configured, the last command returns:

```json
{"providers":["google","microsoft","github"]}
```

Reload the updated local client page. Choose a sign-in button, complete sign-in in its popup, and return to the client. Allow popups for its localhost page if your browser blocks them. The native client polls for completion; the popup never receives an application session token. Use **Cancel sign-in** to abandon an attempt. A pending attempt expires after ten minutes and survives a page reload while the native client stays running.

Use the same email across providers to access the same application account, balance and welcome grant. Verified identities link by provider subject, so a later provider email change does not create another account. If a first-time provider requires the email check, use an address you control and want associated with that account.

If a provider reports a redirect mismatch, compare the registered callback with the exact URL above, including `https`, the provider name and `/callback`, with no trailing slash. Failures reaching the backend are visible in the password-protected admin **Diagnostics** section under `social_sign_in`; provider-side setup errors may only appear in the provider window.
