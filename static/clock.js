"use strict";

// Everything on the page is rendered by the server and swapped in by htmx.
// This only keeps the clock running between two server updates: countdowns,
// the timer bar, and closing the answer form when time is up. Deadlines are
// server times and every view carries the server's `now`, so each phone
// counts down to the same instant however wrong its own clock is.

let offset = 0;
let seenNow = null;

function tick() {
  const view = document.querySelector("[data-now]");
  if (!view) return;
  if (view.dataset.now !== seenNow) {
    seenNow = view.dataset.now;
    offset = Number(seenNow) - Date.now();
  }
  const now = Date.now() + offset;
  const left = (el) => Math.max(0, Number(el.dataset.ends) - now);

  const bar = document.getElementById("bar");
  if (bar && bar.dataset.ends) {
    const span = Number(bar.dataset.ends) - Number(bar.dataset.started);
    bar.style.width = `${(100 * left(bar)) / span}%`;
    bar.classList.toggle("hurry", left(bar) < 5000);
  }
  for (const el of document.querySelectorAll(".countdown[data-ends]")) {
    el.textContent = el.dataset.label.replace("{}", Math.ceil(left(el) / 1000));
  }
  const choices = document.querySelector("fieldset[data-closes]");
  if (choices && now >= Number(choices.dataset.closes)) choices.disabled = true;
}

setInterval(tick, 200);
document.addEventListener("DOMContentLoaded", tick);
document.addEventListener("htmx:afterSettle", tick);

const connection = () => document.getElementById("connection");
document.addEventListener("htmx:sseOpen", () => {
  connection().textContent = "";
});
document.addEventListener("htmx:sseError", () => {
  connection().textContent = "reconnecting…";
});

// A pick that didn't reach the server: say so and show the answer the server
// actually holds, so the screen never claims something that isn't saved.
function answerFailed(event, text) {
  const form = event.detail.elt;
  if (form.id !== "answer") return;
  for (const radio of form.querySelectorAll("input[name=choice]")) {
    radio.checked = radio.value === form.dataset.saved;
  }
  form.querySelector(".notice").textContent = text;
}

document.addEventListener("htmx:afterRequest", (event) => {
  const form = event.detail.elt;
  if (form.id !== "answer" || !event.detail.successful) return;
  form.dataset.saved = event.detail.requestConfig.formData.get("choice");
  form.querySelector(".notice").textContent = "";
});
document.addEventListener("htmx:sendError", (event) =>
  answerFailed(event, "Network trouble, pick again"),
);
document.addEventListener("htmx:responseError", (event) =>
  answerFailed(event, event.detail.xhr.status === 409 ? "Too late!" : "Not saved, pick again"),
);
