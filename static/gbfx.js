// Game Boy power transition: the screen collapses to a thin line then a dot when leaving a page
// (gbGo), and expands back open when a page loads (auto). Include in <head>; navigate with
// gbGo(url) instead of location.href to get the "power off -> power on" effect between screens.
(function () {
  var css =
    'html.gb-off body, html.gb-boot body { will-change: transform, filter; }' +
    'html.gb-off body { animation: gb-off .42s ease-in forwards; transform-origin: 50% 50%; }' +
    'html.gb-boot body { animation: gb-boot .48s ease-out; transform-origin: 50% 50%; }' +
    '@keyframes gb-off {' +
    '  0%   { transform: scaleY(1)     scaleX(1); filter: brightness(1);   }' +
    '  48%  { transform: scaleY(0.016) scaleX(1); filter: brightness(2.2); }' +
    '  100% { transform: scaleY(0.016) scaleX(0); filter: brightness(3);   opacity: .3; }' +
    '}' +
    '@keyframes gb-boot {' +
    '  0%   { transform: scaleY(0.016) scaleX(0); filter: brightness(3);   }' +
    '  40%  { transform: scaleY(0.016) scaleX(1); filter: brightness(2);   }' +
    '  100% { transform: scaleY(1)     scaleX(1); filter: brightness(1);   }' +
    '}' +
    '#gbflash { position: fixed; inset: 0; background: #cfd6c2; opacity: 0; pointer-events: none; z-index: 99999; }' +
    'html.gb-off #gbflash { animation: gb-flash .42s ease-in forwards; }' +
    '@keyframes gb-flash { 0%{opacity:0;} 50%{opacity:0;} 100%{opacity:.7;} }';
  var style = document.createElement('style');
  style.textContent = css;
  (document.head || document.documentElement).appendChild(style);

  // Power ON when this page arrives.
  var de = document.documentElement;
  de.classList.add('gb-boot');
  setTimeout(function () { de.classList.remove('gb-boot'); }, 500);

  document.addEventListener('DOMContentLoaded', function () {
    var f = document.createElement('div');
    f.id = 'gbflash';
    document.body.appendChild(f);
  });

  // Power OFF, then navigate (the next page powers on).
  window.gbGo = function (url) {
    de.classList.add('gb-off');
    setTimeout(function () { location.href = url; }, 400);
  };
})();
