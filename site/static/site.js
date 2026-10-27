// Progressive enhancement: without JS, visitors can still choose either platform.
const body = document.body;
const picker = document.querySelector('.download-picker');
const viewButtons = document.querySelectorAll('[data-view]');
const noteTabs = document.querySelector('.note-tabs');
if (noteTabs) noteTabs.hidden = false;
viewButtons.forEach((button) => {
  button.addEventListener('click', () => {
    const notesSelected = button.dataset.view === 'notes';
    document.getElementById('notes-panel').hidden = !notesSelected;
    document.getElementById('transcript-panel').hidden = notesSelected;
    viewButtons.forEach((item) => item.setAttribute('aria-pressed', String(item === button)));
  });
});

// Check mobile first: iPad can report MacIntel; Android often reports Linux.
const agent = navigator.userAgent;
const platform = navigator.userAgentData?.platform || navigator.platform || '';
const mobile = navigator.userAgentData?.mobile || /Android|iPhone|iPad|iPod/i.test(agent)
  || (/Mac/i.test(platform) && navigator.maxTouchPoints > 1);
const os = mobile ? 'other' : /Mac/i.test(platform || agent) ? 'macos'
  : /Linux/i.test(platform || agent) && !/CrOS/i.test(agent) ? 'linux' : 'other';
document.documentElement.dataset.os = os;

if (os !== 'other') {
  const label = os === 'macos' ? 'Download' : 'Get it';
  const icon = os === 'macos' ? body.dataset.appleIcon : body.dataset.linuxIcon;
  document.querySelectorAll('[data-os-label]').forEach((element) => { element.textContent = label; });
  document.querySelectorAll('[data-os-icon]').forEach((element) => {
    const image = document.createElement('img');
    image.src = icon;
    image.alt = '';
    image.width = 17;
    image.height = 17;
    element.replaceChildren(image);
    element.hidden = false;
  });
  const hint = document.querySelector('[data-os-hint]');
  if (hint) hint.textContent = os === 'macos' ? 'for macOS' : 'for Linux';
}

const macUrl = body.dataset.macosUrl;
const headerAction = document.querySelector('.nav-download');
const directDownload = document.querySelector('.os-download');
if (os === 'macos') {
  document.querySelectorAll('[data-os-action]').forEach((link) => { link.href = macUrl; });
  if (directDownload) directDownload.hidden = false;
  if (picker) {
    // Keep an explicit way to choose Linux even when detection selects macOS.
    picker.classList.add('alternate-platforms');
    const summary = picker.querySelector('summary');
    summary.className = 'other-platforms';
    summary.replaceChildren(document.createTextNode('Other platforms'));
  }
}

function openPicker() {
  picker.open = true;
  picker.querySelector('summary').focus();
  picker.scrollIntoView({ block: 'nearest', behavior: 'instant' });
}
if (picker && os !== 'macos') {
  headerAction?.addEventListener('click', (event) => {
    event.preventDefault();
    openPicker();
  });
}

if (picker) {
  document.addEventListener('click', (event) => {
    if (picker.open && !picker.contains(event.target) && !event.target.closest('.nav-download')) picker.open = false;
  });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape' && picker.open) {
      picker.open = false;
      picker.querySelector('summary').focus();
    }
  });
}
