// The finding-aid editor: while a draft is being made, say so and stop a
// second request. A model can take a minute to answer, and without this the
// only sign of life is the browser's own loading indicator, while the button
// stays live for a second, duplicate request. The form works without it.
// Its sentences come from i18n.js's `t()`, loaded just before it.
(() => {
  const form = document.querySelector('form.manage-form');
  const button = form && form.querySelector('button.draft');
  if (!button) return;
  const status = document.getElementById('draft-status');
  let sent = false;

  form.addEventListener('submit', (event) => {
    if (sent) {
      event.preventDefault();
      return;
    }
    if (event.submitter !== button) return;
    sent = true;
    const service = button.dataset.service || t('js-draft-model-default');
    const limit = Number(button.dataset.timeout) || 120;
    const started = Date.now();
    // Next tick, so the submission has already taken the button's formaction:
    // a button disabled in time would drop out of the request.
    setTimeout(() => {
      form.querySelectorAll('button').forEach((b) => { b.disabled = true; });
      button.textContent = t('js-drafting');
      if (!status) return;
      status.hidden = false;
      const tick = () => {
        const s = Math.round((Date.now() - started) / 1000);
        status.textContent = t('js-draft-waiting', { service, s, limit });
      };
      tick();
      setInterval(tick, 1000);
    }, 0);
  });

  // Coming back to this page from the browser's history restores it as it
  // was, buttons disabled; put them back.
  window.addEventListener('pageshow', (event) => {
    if (!event.persisted) return;
    sent = false;
    form.querySelectorAll('button').forEach((b) => { b.disabled = false; });
    if (status) status.hidden = true;
  });
})();
