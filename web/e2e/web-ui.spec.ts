import { expect, test, type Page } from "@playwright/test";

import { installMockKiliax } from "./fixtures/mock-kiliax";

function lastRequest<T extends { method: string; path: string }>(
  requests: T[],
  predicate: (request: T) => boolean,
): T | undefined {
  for (let i = requests.length - 1; i >= 0; i -= 1) {
    if (predicate(requests[i])) return requests[i];
  }
  return undefined;
}

async function openSidebarIfCollapsed(page: Page) {
  const show = page.getByRole("button", { name: "Show sidebar" });
  if (await show.isVisible()) {
    await show.click();
  }
}

test("renders sessions, selected history, workspace controls, and single-line status badges", async ({ page }) => {
  const mock = await installMockKiliax(page);
  const work = mock.sessions.get("s-work")!;
  work.status.run_state = "running";
  work.status.active_run_id = "run-active";
  await page.goto("/sessions/s-work");

  await expect(page.getByText("Workspace thread").first()).toBeVisible();
  await expect(page.getByText("Summarize the repository.")).toBeVisible();
  await expect(page.getByText("This repository contains a Rust control plane and a React Web UI.")).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Agent" })).toHaveValue("master");
  await expect(page.getByRole("combobox", { name: "Model" })).toHaveValue("openai/gpt-4.1-mini");
  await expect(page.getByText("kiliax").first()).toBeVisible();

  const badge = page.getByText("step 12").first();
  await expect(badge).toBeVisible();
  await expect
    .poll(async () =>
      badge.evaluate((el) => {
        const style = window.getComputedStyle(el);
        return {
          whiteSpace: style.whiteSpace,
          height: el.getBoundingClientRect().height,
          lineHeight: Number.parseFloat(style.lineHeight),
        };
      }),
    )
    .toMatchObject({ whiteSpace: "nowrap" });
});

test("creates a session from the empty composer and renders live run output", async ({ page }) => {
  const mock = await installMockKiliax(page);
  await page.goto("/");

  await expect(page.getByText("Let's cook")).toBeVisible();
  await page.getByPlaceholder(/Ask anything/).fill("Write a compact project summary");
  await page.getByRole("button", { name: "Send" }).click();

  await expect(page).toHaveURL(/\/sessions\/s-new-/);
  await expect(page.getByText("Write a compact project summary")).toBeVisible();
  const runRequest = mock.requests.find((r) => r.method === "POST" && r.path.match(/\/runs$/));
  expect(runRequest?.body.input.text).toBe("Write a compact project summary");
  expect(runRequest?.body.auto_resume).toBe(true);

  const sessionId = page.url().match(/\/sessions\/([^/?#]+)/)?.[1]!;
  mock.messages.set(sessionId, [
    {
      role: "user",
      id: "1",
      created_at: "2026-05-30T09:00:01.000Z",
      content: "Write a compact project summary",
    },
    {
      role: "assistant",
      id: "2",
      created_at: "2026-05-30T09:00:02.000Z",
      content: "Kiliax combines Rust agent runtime services with a React web client.",
    },
  ]);

  await mock.emitWs({ type: "assistant_delta", event_id: 1, run_id: "run-20", data: { delta: "Kiliax combines " } });
  await expect(page.getByText("Kiliax combines")).toBeVisible();
  await mock.emitWs({ type: "run_done", event_id: 2, run_id: "run-20", data: { run: { id: "run-20" } } });
  await expect(page.getByText("Kiliax combines Rust agent runtime services with a React web client.")).toBeVisible();
});

test("sends composer attachments in the run payload", async ({ page }) => {
  const mock = await installMockKiliax(page);
  await page.goto("/sessions/s-work");

  await page.locator('input[type="file"]').setInputFiles({
    name: "brief.pdf",
    mimeType: "application/pdf",
    buffer: Buffer.from("%PDF-1.4\n"),
  });
  await expect(page.getByText("brief.pdf")).toBeVisible();
  await page.getByPlaceholder(/Ask anything/).fill("Read this attachment");
  await page.getByRole("button", { name: "Send" }).click();

  const run = lastRequest(mock.requests, (r) => r.method === "POST" && r.path === "/v1/sessions/s-work/runs");
  expect(run?.body.input.text).toBe("Read this attachment");
  expect(run?.body.input.attachments).toEqual([
    {
      filename: "brief.pdf",
      media_type: "application/pdf",
      data: Buffer.from("%PDF-1.4\n").toString("base64"),
    },
  ]);
});

test("sets and clears a goal, and adds an extra workspace folder through the server-side picker", async ({ page }) => {
  const mock = await installMockKiliax(page);
  await page.goto("/sessions/s-work");

  await page.getByPlaceholder("Set a persistent goal...").fill("Ship complete web e2e coverage");
  await page.getByRole("button", { name: "Set", exact: true }).click();
  await expect(page.getByText("Status: active")).toBeVisible();
  expect(mock.requests.find((r) => r.method === "PUT" && r.path === "/v1/sessions/s-work/goal")?.body.objective)
    .toBe("Ship complete web e2e coverage");

  await page.getByRole("button", { name: "Clear" }).click();
  await expect(page.getByText("No active goal")).toBeVisible();

  await page.getByRole("button", { name: "Add folder" }).click();
  const addFolder = page.getByRole("dialog", { name: "Add folder" });
  await expect(addFolder).toBeVisible();
  await addFolder.getByRole("button", { name: "fixtures" }).click();
  await addFolder.getByRole("button", { name: "Add" }).click();
  await expect(addFolder).toBeHidden();
  const patch = lastRequest(mock.requests, (r) => r.method === "PATCH" && r.path === "/v1/sessions/s-work/settings");
  expect(patch?.body.extra_workspace_roots).toContain("D:\\fixtures");
});

test("edits provider settings and raw yaml from Settings", async ({ page }) => {
  const mock = await installMockKiliax(page);
  await page.goto("/sessions/s-work");

  await openSidebarIfCollapsed(page);
  await page.getByRole("button", { name: "Settings" }).click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings).toBeVisible();

  await settings.getByText("Add provider").first().click();
  await settings.getByPlaceholder("openai", { exact: true }).fill("local");
  await settings.getByPlaceholder("https://api.openai.com/v1", { exact: true }).fill("http://127.0.0.1:11434/v1");
  await settings.getByPlaceholder(/Add model/).fill("llama3.1");
  await settings.getByRole("button", { name: "Add", exact: true }).click();
  await expect(settings.getByText("llama3.1")).toBeVisible();
  await settings.locator("button").filter({ hasText: /^Add provider$/ }).last().click();
  await expect(settings.getByText("local").first()).toBeVisible();
  expect(mock.providers.some((p) => p.id === "local" && p.models.some((m: any) => m.id === "llama3.1"))).toBe(true);

  await settings.getByRole("button", { name: "Agents" }).click();
  await settings.getByPlaceholder("default: 1024").fill("128");
  await settings.getByRole("button", { name: "Save" }).click();
  expect(lastRequest(mock.requests, (r) => r.method === "PATCH" && r.path === "/v1/config/runtime")?.body.runtime_max_steps)
    .toBe(128);

  await settings.getByRole("button", { name: "Raw YAML" }).click();
  const yaml = settings.locator("textarea");
  await expect(yaml).toHaveValue(/default_model/);
  await yaml.fill("default_model: local/llama3.1\n");
  await settings.getByRole("button", { name: "Save" }).click();
  expect(lastRequest(mock.requests, (r) => r.method === "PUT" && r.path === "/v1/config")?.body.yaml)
    .toBe("default_model: local/llama3.1\n");
});
