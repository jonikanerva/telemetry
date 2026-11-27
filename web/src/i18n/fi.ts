// All Finnish text of the user interface. Components take their text from
// this object and contain no literal user-visible strings.
export const fi = {
  appName: 'Telemetria',
  skipToContent: 'Siirry sisältöön',
  footerTimeZone: 'Ajat ovat Suomen aikaa (Europe/Helsinki).',
  notFoundTitle: 'Sivua ei löydy',
  notFoundBody: 'Osoitteessa ei ole sivua.',
  notFoundLink: 'Etusivulle',
  homeTitle: 'Palvelun tila',
  refresh: 'Päivitä',
  healthLoading: 'Haetaan palvelun tilaa…',
  healthAvailable: 'Palvelu toimii.',
  healthVersion: 'Palvelimen versio',
  versionUnknown: 'tuntematon',
  unavailableTitle: 'Tietoja ei saatu',
  unavailableBody: 'Palvelu ei vastannut oikein. Yritä myöhemmin uudelleen.',
  reloadBanner: 'Sovelluksesta on uusi versio.',
  reloadButton: 'Lataa sivu uudelleen',
} as const;
