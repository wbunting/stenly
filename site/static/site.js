// Progressive enhancement: the download link and Linux note work without JavaScript.
const body = document.body;
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

// Only macOS has a download. The button always reads "Download for macOS" and links to the
// macOS asset (set in the template, so it works without JavaScript); JS just adds the Apple
// icon on Macs. There is no Linux asset yet, so Linux visitors see the "Linux: coming soon"
// note with pre-release guides instead of a dead link.
if (os === 'macos') {
  document.querySelectorAll('[data-os-icon]').forEach((element) => {
    const image = document.createElement('img');
    image.src = body.dataset.appleIcon;
    image.alt = '';
    image.width = 17;
    image.height = 17;
    element.replaceChildren(image);
    element.hidden = false;
  });
}
