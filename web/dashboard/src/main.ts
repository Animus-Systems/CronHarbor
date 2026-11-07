import { createApp } from "vue";
import { createRouter, createWebHashHistory } from "vue-router";
import App from "./App.vue";
import OverviewView from "./views/OverviewView.vue";
import WorkloadsView from "./views/WorkloadsView.vue";
import WorkloadView from "./views/WorkloadView.vue";
import EditorView from "./views/EditorView.vue";
import ExecutionsView from "./views/ExecutionsView.vue";
import ExecutionView from "./views/ExecutionView.vue";
import OperationsView from "./views/OperationsView.vue";
import SettingsView from "./views/SettingsView.vue";
import "./style.css";
const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/", component: OverviewView, meta: { title: "Overview" } },
    {
      path: "/jobs",
      component: WorkloadsView,
      props: { kind: "jobs" },
      meta: { title: "Jobs" },
    },
    {
      path: "/jobs/new",
      component: EditorView,
      meta: { title: "Create workload" },
    },
    {
      path: "/jobs/:id/edit",
      component: EditorView,
      meta: { title: "Edit definition" },
    },
    {
      path: "/jobs/:id",
      component: WorkloadView,
      meta: { title: "Job details" },
    },
    {
      path: "/services",
      component: WorkloadsView,
      props: { kind: "services" },
      meta: { title: "Services" },
    },
    {
      path: "/services/:id",
      component: WorkloadView,
      meta: { title: "Service details" },
    },
    {
      path: "/executions",
      component: ExecutionsView,
      meta: { title: "Executions" },
    },
    {
      path: "/executions/:id",
      component: ExecutionView,
      meta: { title: "Execution details" },
    },
    {
      path: "/logs",
      component: ExecutionsView,
      props: { logsMode: true },
      meta: { title: "Logs" },
    },
    {
      path: "/operations",
      component: OperationsView,
      meta: { title: "Operations" },
    },
    {
      path: "/operations/:id",
      component: OperationsView,
      meta: { title: "Operation details" },
    },
    {
      path: "/settings",
      component: SettingsView,
      meta: { title: "Settings & access" },
    },
    { path: "/:pathMatch(.*)*", redirect: "/" },
  ],
  scrollBehavior: () => ({ top: 0 }),
});
router.afterEach((to) => {
  requestAnimationFrame(() =>
    document.getElementById("main-content")?.focus({ preventScroll: true }),
  );
  document.title = `${String(to.meta.title ?? "Console")} · CronHarbor`;
});
createApp(App).use(router).mount("#app");
