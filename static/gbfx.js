// Scene transition: a smooth opacity cross-fade between screens (cinematic, no flicker). Each page
// fades IN on load; navigate with gbGo(url) to fade OUT then go (the next page fades back in).
(function () {
  var css =
    'body { transition: opacity .32s ease; }' +
    'html.gb-out body { opacity: 0; }';
  var style = document.createElement('style');
  style.textContent = css;
  (document.head || document.documentElement).appendChild(style);

  // Start hidden (no flash of content), then fade in once the page is ready.
  var de = document.documentElement;
  de.classList.add('gb-out');
  function fadeIn() { requestAnimationFrame(function () { de.classList.remove('gb-out'); }); }
  if (document.readyState !== 'loading') fadeIn();
  else document.addEventListener('DOMContentLoaded', fadeIn);

  // Fade out, then navigate (the destination fades itself back in).
  window.gbGo = function (url) {
    de.classList.add('gb-out');
    setTimeout(function () { location.href = url; }, 330);
  };
})();
