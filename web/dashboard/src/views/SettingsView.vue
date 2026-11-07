<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { dashboard as d } from "../store";
import { formatError } from "../api";
import { isToken, isAudit, isCreatedToken, isValid, pageOf } from "../guards";
import type { AuditDto, Role, TokenDto } from "../generated/contracts";
import { time } from "../format";
import AdapterHealth from "../components/AdapterHealth.vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
const tokens = ref<TokenDto[]>([]);
const tokenCursor = ref<string | null>(null);
const audit = ref<AuditDto[]>([]);
const auditCursor = ref<string | null>(null);
const tokenError = ref("");
const auditError = ref("");
const formError = ref("");
const busy = ref(false);
const loading = ref(true);
const role = ref<Role>("viewer");
const namespaces = ref("default");
const capabilities = ref("workload.read\nexecution.read");
const ttlHours = ref(8);
const secret = ref("");
const reveal = ref(false);
const copied = ref(false);
const action = ref("");
const revoke = ref<TokenDto>();
let stopped = false;
const canManage = computed(() => d.can("tokens.manage"));
const canAudit = computed(
  () =>
    d.can("operations.manage") &&
    Boolean(d.principal.value?.namespaces.includes("*")),
);
const split = (text: string) => [
  ...new Set(
    text
      .split(/[\s,]+/)
      .map((s) => s.trim())
      .filter(Boolean),
  ),
];
async function loadTokens(more = false): Promise<void> {
  try {
    const result = await d
      .client()
      .get(
        `/tokens?limit=100${more && tokenCursor.value ? `&cursor=${encodeURIComponent(tokenCursor.value)}` : ""}`,
        pageOf(isToken),
      );
    tokens.value = more ? [...tokens.value, ...result.items] : result.items;
    tokenCursor.value = result.next_cursor;
    tokenError.value = "";
  } catch (caught) {
    tokenError.value = formatError(caught);
  }
}
async function loadAudit(more = false): Promise<void> {
  try {
    const result = await d
      .client()
      .get(
        `/audit?limit=50${more && auditCursor.value ? `&cursor=${encodeURIComponent(auditCursor.value)}` : ""}`,
        pageOf(isAudit),
      );
    audit.value = more ? [...audit.value, ...result.items] : result.items;
    auditCursor.value = result.next_cursor;
    auditError.value = "";
  } catch (caught) {
    auditError.value = formatError(caught);
  }
}
onMounted(async () => {
  await Promise.all([
    canManage.value ? loadTokens() : Promise.resolve(),
    canAudit.value ? loadAudit() : Promise.resolve(),
  ]);
  loading.value = false;
});
onBeforeUnmount(() => {
  stopped = true;
  secret.value = "";
});
function prepareCreate(): void {
  formError.value = "";
  if (
    !split(namespaces.value).length ||
    !Number.isInteger(ttlHours.value) ||
    ttlHours.value < 1 ||
    ttlHours.value > 720
  ) {
    formError.value =
      "Supply at least one namespace and a whole lifetime between 1 and 720 hours.";
    return;
  }
  action.value = "create";
}
async function confirm(): Promise<void> {
  const current = d.api.value;
  const requested = action.value;
  action.value = "";
  busy.value = true;
  formError.value = "";
  try {
    if (requested === "create") {
      const result = await d.client().post(
        "/tokens",
        {
          role: role.value,
          namespaces: split(namespaces.value),
          capabilities: split(capabilities.value),
          ttl_seconds: ttlHours.value * 3600,
        },
        isCreatedToken,
      );
      if (stopped || d.api.value !== current) return;
      secret.value = result.token;
      reveal.value = false;
      copied.value = false;
    } else if (revoke.value)
      await d
        .client()
        .post(
          `/tokens/${encodeURIComponent(revoke.value.id)}/revoke`,
          {},
          isValid,
        );
    if (stopped || d.api.value !== current) return;
    await Promise.all([
      loadTokens(),
      canAudit.value ? loadAudit() : Promise.resolve(),
    ]);
  } catch (caught) {
    formError.value = formatError(caught);
  } finally {
    busy.value = false;
  }
}
async function copySecret(): Promise<void> {
  try {
    await navigator.clipboard.writeText(secret.value);
    copied.value = true;
  } catch {
    formError.value =
      "Clipboard access is unavailable. Reveal the token and copy it manually.";
  }
}
</script>
<template>
  <section class="page-heading">
    <div>
      <p class="eyebrow">INSTANCE & TRUST</p>
      <h1>Settings & access<span class="heading-dot">.</span></h1>
      <p class="muted">
        Explicit capabilities. Expiring tokens. Auditable changes.
      </p>
    </div>
    <span class="source-badge">{{
      d.principal.value?.role.toUpperCase()
    }}</span>
  </section>
  <div class="two-column">
    <section class="panel">
      <div class="panel-heading"><h2>Current principal</h2></div>
      <dl class="detail-grid">
        <div>
          <dt>Actor</dt>
          <dd class="mono wrap">{{ d.principal.value?.actor_id }}</dd>
        </div>
        <div>
          <dt>Role</dt>
          <dd>{{ d.principal.value?.role }}</dd>
        </div>
        <div>
          <dt>Namespace scope</dt>
          <dd>{{ d.principal.value?.namespaces.join(", ") || "None" }}</dd>
        </div>
        <div>
          <dt>Session storage</dt>
          <dd>Tab memory only · cleared on reload</dd>
        </div>
      </dl>
      <div class="panel-content">
        <h3>Explicit capabilities</h3>
        <div class="capability-list">
          <span
            v-for="capability in d.principal.value?.capabilities"
            :key="capability"
            class="source-badge mono"
            >{{ capability }}</span
          >
          <p v-if="!d.principal.value?.capabilities.length" class="muted">
            No explicit capability grants returned.
          </p>
        </div>
      </div>
    </section>
    <section class="panel">
      <div class="panel-heading"><h2>Instance boundaries</h2></div>
      <dl class="detail-grid">
        <div>
          <dt>API endpoint</dt>
          <dd class="mono wrap">{{ d.api.value?.base }}</dd>
        </div>
        <div>
          <dt>Dispatch</dt>
          <dd>
            {{
              d.status.value?.restore_required
                ? "Restore quarantine"
                : d.status.value?.paused
                  ? "Paused"
                  : "Enabled"
            }}
          </dd>
        </div>
        <div>
          <dt>SQLite</dt>
          <dd>{{ d.status.value?.sqlite_version }}</dd>
        </div>
        <div>
          <dt>Owner model</dt>
          <dd>Single active daemon</dd>
        </div>
      </dl>
      <div class="panel-content">
        <p class="hint">
          Retention, backup, restore and executor allowlists are administered
          through deployment configuration and the trusted CLI. Verify backups
          before restoring; restoring requires explicit side-effect
          reconciliation.
        </p>
        <p class="hint">
          MCP remains observation-only. Log content and agent-proposed text
          cannot approve commands.
        </p>
      </div>
    </section>
  </div>
  <AdapterHealth />
  <div v-if="!canManage" class="message warning">
    Token administration requires tokens.manage.
  </div>
  <template v-else
    ><p v-if="loading" role="status">
      Loading scoped tokens and audit records…
    </p>
    <section class="panel">
      <div class="panel-heading">
        <div>
          <h2>API tokens</h2>
          <p>
            Only metadata is returned for existing tokens. Secrets are shown
            once at creation.
          </p>
        </div>
        <button
          :disabled="busy || !d.connected.value"
          @click="loadTokens(false)"
        >
          Refresh tokens
        </button>
      </div>
      <p v-if="tokenError" class="message danger" role="alert">
        {{ tokenError }}
      </p>
      <div v-else-if="!tokens.length" class="inline-empty">
        No token metadata available.
      </div>
      <div v-else class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Token / actor</th>
              <th>Role / scope</th>
              <th>Expires · local</th>
              <th>State</th>
              <th>Action</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="token in tokens" :key="token.id">
              <td class="mono">
                {{ token.id }}<small>{{ token.actor_id }}</small>
              </td>
              <td>
                {{ token.role }}<small>{{ token.namespaces.join(", ") }}</small>
                <details>
                  <summary>Capabilities</summary>
                  <ul>
                    <li
                      v-for="capability in token.capabilities"
                      :key="capability"
                      class="mono"
                    >
                      {{ capability }}
                    </li>
                  </ul>
                </details>
              </td>
              <td>{{ time(token.expires_at_us) }}</td>
              <td>
                {{
                  token.revoked
                    ? "Revoked"
                    : token.expires_at_us &&
                        token.expires_at_us < Date.now() * 1000
                      ? "Expired"
                      : "Active"
                }}
              </td>
              <td>
                <button
                  :disabled="token.revoked || busy || !d.canAct.value"
                  class="danger-button"
                  @click="
                    revoke = token;
                    action = 'revoke';
                  "
                >
                  Revoke
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-if="tokenCursor" class="panel-bottom">
        <button @click="loadTokens(true)">Load more token metadata</button>
      </div>
    </section>
    <section class="panel">
      <div class="panel-heading">
        <div>
          <h2>Create scoped token</h2>
          <p>Grant the smallest namespace and capabilities required.</p>
        </div>
      </div>
      <form class="token-form" @submit.prevent="prepareCreate">
        <label class="field"
          >Role<select v-model="role">
            <option value="viewer">Viewer</option>
            <option value="operator">Operator</option>
            <option value="editor">Editor</option>
            <option value="admin">Admin</option>
            <option value="observe">MCP observe</option>
          </select></label
        ><label class="field"
          >Lifetime · hours<input
            v-model.number="ttlHours"
            type="number"
            min="1"
            max="720"
            step="1"
            required /></label
        ><label class="field"
          >Namespaces · separated by commas<input
            v-model="namespaces"
            required
            maxlength="2048"
            placeholder="production, staging" /></label
        ><label class="field span-two"
          >Capabilities · one per line<textarea
            v-model="capabilities"
            rows="5"
            maxlength="4096"
            spellcheck="false"
          />
        </label>
        <p class="hint span-two">
          Examples: workload.read, execution.read, logs.read,
          workload.run_preapproved, service.control_preapproved. Executable
          changes require executor.create_command; privileged adapters need
          their own explicit grant.
        </p>
        <p v-if="formError" class="message danger span-two" role="alert">
          {{ formError }}
        </p>
        <button class="primary" :disabled="busy || !d.canAct.value">
          Review token grant
        </button>
      </form>
    </section>
    <section v-if="secret" class="panel secret-panel">
      <div class="panel-heading">
        <div>
          <h2>Copy the new token now</h2>
          <p>
            This secret is kept in tab memory until cleared or this screen is
            left. It cannot be retrieved again.
          </p>
        </div>
        <button
          @click="
            secret = '';
            reveal = false;
          "
        >
          Clear secret
        </button>
      </div>
      <div class="panel-content">
        <label class="field"
          >One-time token<input
            :value="secret"
            :type="reveal ? 'text' : 'password'"
            readonly
            autocomplete="off"
            spellcheck="false"
        /></label>
        <div class="row-actions">
          <button :aria-pressed="reveal" @click="reveal = !reveal">
            {{ reveal ? "Hide token" : "Reveal token" }}</button
          ><button @click="copySecret">
            {{ copied ? "Copied" : "Copy token" }}
          </button>
        </div>
      </div>
    </section>
  </template>
  <p v-if="!canAudit" class="message warning">
    Instance-wide audit requires operations.manage and wildcard namespace
    authority.
  </p>
  <section v-else class="panel">
    <div class="panel-heading">
      <div>
        <h2>Audit trail</h2>
        <p>Actor, action, target and server request ID · secrets excluded</p>
      </div>
      <button :disabled="!d.connected.value" @click="loadAudit(false)">
        Refresh audit
      </button>
    </div>
    <p v-if="auditError" class="message danger" role="alert">
      {{ auditError }}
    </p>
    <div v-else-if="!audit.length" class="inline-empty">
      No audit records returned.
    </div>
    <div v-else class="table-wrap">
      <table>
        <thead>
          <tr>
            <th>Time · UTC</th>
            <th>Actor</th>
            <th>Action / target</th>
            <th>Result</th>
            <th>Request ID</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in audit" :key="row.seq">
            <td>{{ time(row.created_at_us, true) }}</td>
            <td class="mono">{{ row.actor_id }}</td>
            <td>
              {{ row.action
              }}<small class="mono">{{ row.target_id ?? "Instance" }}</small>
            </td>
            <td>{{ row.result }}</td>
            <td class="mono">{{ row.request_id }}</td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-if="auditCursor" class="panel-bottom">
      <button @click="loadAudit(true)">Load next audit page</button>
    </div>
  </section>
  <ConfirmDialog
    :open="Boolean(action)"
    :title="
      action === 'create' ? 'Create this token grant?' : 'Revoke this token?'
    "
    :impact="
      action === 'create'
        ? `A ${role} token will grant the listed capabilities in ${namespaces} for ${ttlHours} hours. Review every capability, especially command creation and adapter management.`
        : 'Revocation blocks subsequent requests and terminates authorized streams. If this is your current token, this console will disconnect.'
    "
    :confirm-label="
      action === 'create' ? 'Create scoped token' : 'Revoke token'
    "
    @cancel="action = ''"
    @confirm="confirm"
  />
</template>
