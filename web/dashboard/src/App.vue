<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount, ref, nextTick } from "vue";
import { useRoute, useRouter } from "vue-router";
import { dashboard as d } from "./store";
import { formatError, loadApiBase } from "./api";
const route = useRoute();
const router = useRouter();
const token = ref("");
const base = ref("");
const configurationError = ref("");
const loginError = ref("");
const menuOpen = ref(false);
const title = computed(() => String(route.meta.title ?? "Overview"));
const nav = [
  {
    path: "/",
    label: "Overview",
    icon: "M3 3h7v7H3zM14 3h7v7h-7zM3 14h7v7H3zM14 14h7v7h-7z",
  },
  {
    path: "/jobs",
    label: "Jobs",
    icon: "M9 4H4v16h16V4h-5M9 2h6v5H9zM8 12h8M8 16h5",
  },
  {
    path: "/services",
    label: "Services",
    icon: "M3 6h18v5H3zM3 15h18v5H3zM7 8.5h.01M7 17.5h.01",
  },
  { path: "/executions", label: "Executions", icon: "m9 5 11 7-11 7zM4 4v16" },
  { path: "/logs", label: "Logs", icon: "M4 5h16M4 10h16M4 15h9M4 20h12" },
  { path: "/operations", label: "Operations", icon: "M3 12h5l3-8 4 16 3-8h3" },
  {
    path: "/settings",
    label: "Settings & access",
    icon: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v3M12 19v3M2 12h3M19 12h3M5 5l2 2M17 17l2 2M5 19l2-2M17 7l2-2",
  },
];
onMounted(async () => {
  window.addEventListener("offline", d.networkOffline);
  window.addEventListener("online", d.networkOnline);
  try {
    base.value = await loadApiBase();
  } catch {
    configurationError.value =
      "The trusted API configuration is invalid. Use an HTTPS API address or localhost in config.json.";
  }
});
onBeforeUnmount(() => {
  window.removeEventListener("offline", d.networkOffline);
  window.removeEventListener("online", d.networkOnline);
});
async function login(): Promise<void> {
  loginError.value = "";
  try {
    await d.connect(base.value, token.value.trim());
    token.value = "";
    await nextTick();
    document.getElementById("main-content")?.focus();
  } catch (error) {
    loginError.value = formatError(error);
  }
}
function skipToContent(): void {
  document.getElementById("main-content")?.focus();
}
function logout(): void {
  d.disconnect();
  token.value = "";
  void router.push("/");
}
async function refresh(): Promise<void> {
  await d.refresh().catch(() => {});
}
</script>
<template>
  <a class="skip-link" href="#main-content" @click.prevent="skipToContent"
    >Skip to main content</a
  >
  <div v-if="!d.principal.value" class="connection-screen">
    <div class="connection-brand">
      <svg viewBox="0 0 40 40" aria-hidden="true">
        <path
          d="M9 8h22v24H9zM4 14h10M26 26h10M20 4v10M20 26v10"
          fill="none"
          stroke="currentColor"
          stroke-width="3"
        />
        <path
          d="m15 20 4 4 7-8"
          fill="none"
          stroke="currentColor"
          stroke-width="3"
        /></svg
      ><span>CronHarbor</span>
    </div>
    <form
      id="main-content"
      tabindex="-1"
      class="connection-card"
      @submit.prevent="login"
    >
      <p class="eyebrow">OPERATOR CONSOLE</p>
      <h1>A clear view of<br />what runs next.</h1>
      <p class="muted">
        Connect to your single-node scheduler to inspect workloads, follow
        executions, and control approved operations.
      </p>
      <div class="endpoint">
        <span class="status-dot" aria-hidden="true" /><span>{{
          base || "Loading trusted endpoint…"
        }}</span>
      </div>
      <label class="field"
        >Access token<input
          v-model="token"
          type="password"
          autocomplete="off"
          spellcheck="false"
          required
          maxlength="1024"
          placeholder="Paste your scoped bearer token"
          :disabled="!base || d.connecting.value"
      /></label>
      <p class="hint">
        Your token stays in this tab’s memory. Closing or reloading the tab
        clears the session.
      </p>
      <p
        v-if="
          loginError ||
          configurationError ||
          d.error.value ||
          d.connectionError.value
        "
        class="message danger"
        role="alert"
      >
        {{
          loginError ||
          configurationError ||
          d.error.value ||
          d.connectionError.value
        }}
      </p>
      <button
        type="submit"
        class="primary full"
        :disabled="!base || d.connecting.value"
      >
        {{ d.connecting.value ? "Connecting…" : "Connect to instance"
        }}<span aria-hidden="true">→</span>
      </button>
      <p class="connection-foot">
        SCOPED ACCESS <span>·</span> NO BROWSER TOKEN STORAGE
      </p>
    </form>
    <p class="connection-note">
      One scheduler. Explicit ownership. Observable outcomes.
    </p>
  </div>
  <div v-else class="app-shell">
    <aside class="sidebar" :class="{ 'menu-open': menuOpen }">
      <RouterLink to="/" class="brand" @click="menuOpen = false"
        ><svg viewBox="0 0 32 32" aria-hidden="true">
          <path d="M7 6h18v20H7zM3 11h8M21 21h8M16 2v9M16 21v9" />
          <path d="m11 16 4 4 7-8" /></svg
        ><span>CronHarbor<small>OPERATOR CONSOLE</small></span></RouterLink
      >
      <p class="nav-label">WORKSPACE</p>
      <nav aria-label="Main navigation">
        <RouterLink
          v-for="item in nav"
          :key="item.path"
          :to="item.path"
          :class="{
            active:
              item.path === '/'
                ? route.path === '/'
                : route.path.startsWith(item.path),
          }"
          @click="menuOpen = false"
          ><svg viewBox="0 0 24 24" aria-hidden="true">
            <path :d="item.icon" /></svg
          >{{ item.label
          }}<span
            v-if="item.path === '/executions' && d.status.value?.unknown"
            class="nav-count"
            >{{ d.status.value.unknown }}</span
          ></RouterLink
        >
      </nav>
      <div class="sidebar-bottom">
        <div class="instance-icon" aria-hidden="true">
          <svg viewBox="0 0 24 24"><path d="M4 5h16v14H4zM8 9h8M8 13h5" /></svg>
        </div>
        <div class="min-width-zero">
          <strong>Single-node instance</strong
          ><small>{{
            d.connected.value ? "Connection verified" : "Connection unavailable"
          }}</small>
        </div>
        <span
          class="status-dot"
          :class="{ disconnected: !d.connected.value }"
          aria-hidden="true"
        />
      </div>
    </aside>
    <div class="workspace">
      <header class="topbar">
        <div class="breadcrumb">
          <button
            class="menu-toggle icon-button"
            :aria-expanded="menuOpen"
            aria-label="Toggle navigation"
            @click="menuOpen = !menuOpen"
          >
            <svg viewBox="0 0 24 24" aria-hidden="true">
              <path d="M4 6h16M4 12h16M4 18h16" />
            </svg></button
          ><span>Workspace</span><span class="divider">/</span
          ><strong>{{ title }}</strong>
        </div>
        <div class="topbar-actions">
          <span
            class="connection-state"
            :class="{ offline: !d.connected.value }"
            ><span class="status-dot" aria-hidden="true" />{{
              d.connected.value ? "Live connection" : "Disconnected"
            }}</span
          ><button
            class="refresh-button"
            :disabled="d.refreshing.value"
            @click="refresh"
          >
            {{ d.refreshing.value ? "Refreshing…" : "Refresh" }}</button
          ><span class="avatar" aria-hidden="true">{{
            d.principal.value.actor_id.slice(0, 2).toUpperCase()
          }}</span
          ><button class="text-button signout" @click="logout">
            Disconnect
          </button>
        </div>
      </header>
      <main id="main-content" tabindex="-1">
        <div v-if="!d.connected.value" class="message warning" role="status">
          <strong>Disconnected — actions are disabled.</strong> Last verified
          snapshot:
          {{
            d.lastRefresh.value
              ? new Date(d.lastRefresh.value).toLocaleString()
              : "unavailable"
          }}. {{ d.connectionError.value }} Reconnecting and refreshing
          authoritative state.
        </div>
        <div v-if="d.error.value" class="message danger" role="alert">
          {{ d.error.value
          }}<button
            class="text-button"
            aria-label="Dismiss error"
            @click="d.error.value = ''"
          >
            Dismiss
          </button>
        </div>
        <div v-if="d.notice.value" class="message info" role="status">
          {{ d.notice.value }}
          <RouterLink to="/operations">Track operation</RouterLink
          ><button
            class="text-button"
            aria-label="Dismiss notice"
            @click="d.notice.value = ''"
          >
            Dismiss
          </button>
        </div>
        <div v-if="d.uncertain.value" class="message warning" role="alert">
          <strong>Submission outcome is uncertain.</strong> Check Operations
          first. A manual retry reuses the exact request and idempotency
          key.<button
            :disabled="!d.connected.value || d.pending.value"
            @click="d.retryUncertain"
          >
            Retry same intent
          </button>
        </div>
        <RouterView :key="route.path" />
        <footer class="page-footer">
          <span
            >CronHarbor <span class="muted">/</span> Single-node control
            plane</span
          ><span
            >Snapshot
            {{
              d.lastRefresh.value
                ? new Date(d.lastRefresh.value).toLocaleTimeString()
                : "unavailable"
            }}
            <span class="footer-dot">·</span> {{ d.principal.value.role }}</span
          >
        </footer>
      </main>
    </div>
  </div>
</template>
