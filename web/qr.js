// Renders the pairing QR code on the /qr page. The server injects the phone
// URL into the body's data-url attribute; this file stays static so the page
// keeps the strict script-src 'self' CSP (no inline scripts).
(function () {
  var url = document.body.getAttribute("data-url");
  var target = document.getElementById("qr");
  if (!url || !target || typeof QRCode === "undefined") {
    return;
  }
  new QRCode(target, {
    text: url,
    width: 512,
    height: 512,
    correctLevel: QRCode.CorrectLevel.M,
  });
  // Show the URL as plain text too, for users who would rather type it.
  var link = document.getElementById("url");
  if (link) {
    link.textContent = url;
    link.href = url;
  }
})();
