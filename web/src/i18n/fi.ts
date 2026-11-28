// All Finnish text of the user interface. Components take their text from
// this object and contain no literal user-visible strings. A text with a
// value is a function of that value.
export const fi = {
  appName: 'Telemetria',
  skipToContent: 'Siirry sisältöön',
  footerTimeZone: 'Ajat ovat Suomen aikaa (Europe/Helsinki).',
  notFoundTitle: 'Sivua ei löydy',
  notFoundBody: 'Osoitteessa ei ole sivua.',
  notFoundLink: 'Etusivulle',
  refresh: 'Päivitä',
  unavailableTitle: 'Tietoja ei saatu',
  unavailableBody: 'Palvelu ei vastannut oikein. Yritä myöhemmin uudelleen.',
  reloadBanner: 'Sovelluksesta on uusi versio.',
  reloadButton: 'Lataa sivu uudelleen',
  retry: 'Yritä uudelleen',
  loginTitle: 'Kirjaudu sisään',
  loginPasswordLabel: 'Salasana',
  loginSubmit: 'Kirjaudu sisään',
  loginSubmitting: 'Kirjaudutaan…',
  loginInvalidCredentials: 'Salasana on väärä.',
  loginThrottled: (seconds: number): string =>
    `Liian monta kirjautumisyritystä. Yritä uudelleen ${String(seconds)} sekunnin kuluttua.`,
  loginThrottledNoTime: 'Liian monta kirjautumisyritystä. Yritä hetken kuluttua uudelleen.',
  conflictingCookies:
    'Selaimessa on tälle sivustolle kaksi ristiriitaista istuntoevästettä, joten palvelu hylkää pyynnöt. Poista tämän sivuston evästeet selaimen asetuksista ja kirjaudu sitten uudelleen.',
  loginUnavailable: 'Kirjautuminen ei onnistunut. Palvelu ei vastannut. Yritä hetken kuluttua uudelleen.',
  logout: 'Kirjaudu ulos',
  logoutPending: 'Kirjaudutaan ulos…',
  logoutFailedTitle: 'Uloskirjautuminen ei onnistunut',
  logoutFailedBody: 'Palvelin ei vahvistanut uloskirjautumista. Istunto voi olla yhä voimassa tällä laitteella.',
  logoutRetry: 'Yritä uloskirjautumista uudelleen',
  // Shared words of the formats (format/format.ts).
  timeWord: 'klo',
  today: 'tänään',
  yesterday: 'eilen',
  durationUnderMinute: 'alle 1 min',

  // Navigation of the shell.
  navLabel: 'Näkymät',
  navBattery: 'Akku',

  // The battery view (battery/).
  batteryTitle: 'Akku',
  batteryLoading: 'Haetaan akun tietoja…',
  refreshing: 'Päivitetään…',
  fetchedAt: (clock: string) => `Haettu klo ${clock}`,
  fetchedAtOverdue: (clock: string) => `Haettu klo ${clock}. Data on myöhässä.`,
  refreshFailed: (clock: string) => `Päivitys epäonnistui. Näytetään aiemmin haettu data (haettu klo ${clock}).`,
  loadFailed: 'Tietoja ei saatu.',
  rangeLabel: 'Aikaväli',
  rangeNames: { '24h': '24 h', '7d': '7 pv', '30d': '30 pv', '90d': '90 pv', '1y': '1 v' },

  // The status line: the stored connection fact and the freshness.
  connectionConnected: (when: string) => `Yhdistetty ${when}`,
  connectionDisconnected: (when: string) => `Ei yhteyttä ${when} alkaen`,
  connectionNone: 'Ei yhteystietoja',
  freshnessOverdue: ', mutta uutta dataa ei ole tallentunut yli 20 minuuttiin.',
  freshnessNotExpected: ': auto nukkuu tai on verkon ulkopuolella.',
  latestData: (when: string) => `Uusin data ${when}.`,

  // Charts.
  chartSocTitle: 'Varaustaso',
  chartSocSubtitle: 'Käytettävissä oleva varaus, %',
  chartCapacityTitle: 'Kapasiteetti',
  chartCapacitySubtitle: 'Akun nimellinen energia täytenä, kWh',
  capacityChange: (delta: string) => `Muutos valitulla välillä: ${delta}.`,
  chartNoDataInRange: 'Ei dataa valitulla välillä.',
  chartCarryIn: (value: string, when: string) => `Viimeisin arvo ennen väliä: ${value} (${when}).`,
  chartSummary: (min: string, max: string, latest: string, when: string) =>
    `Pienin ${min}, suurin ${max}, viimeisin ${latest} (${when}).`,
  chartGapCount: (count: number) =>
    count === 1 ? '1 aukko valitulla välillä.' : `${String(count)} aukkoa valitulla välillä.`,
  chartGapCountAtLeast: (count: number) => `Vähintään ${String(count)} aukkoa valitulla välillä.`,
  chartNoGaps: 'Ei aukkoja valitulla välillä.',
  chartExcluded: (invalid: number, notNumeric: number) =>
    `Hylätyt rivit: ${String(invalid)} ${invalid === 1 ? 'auton virhemerkintä' : 'auton virhemerkintää'}, ${String(notNumeric)} ${notNumeric === 1 ? 'rivi ei ollut kelvollinen luku' : 'riviä ei ollut kelvollinen luku'}.`,
  chartCursorLabel: (title: string) => `${title}: valitse kohta nuolinäppäimillä`,
  chartCursorHint: 'Valitse kohta osoittimella, kosketuksella tai nuolinäppäimillä.',
  chartReadout: (when: string, value: string) => `${when}: ${value}`,
  chartReadoutBucket: (min: string, max: string, count: number) =>
    `vaihteluväli ${min}–${max}, ${String(count)} mittausta`,
  chartLegendLine: 'Mitattu arvo',
  chartLegendGap: 'Aukko: kirjattu datan menetys',
  chartLegendRestart: 'Vastaanottopalvelun käynnistys',
  chartRestartTitle: (when: string) => `Vastaanottopalvelu käynnistyi ${when}`,
  chartBucketNote: (bucket: string) => `Yksi piste on ${bucket} jakson viimeisin arvo.`,
  tableShow: 'Näytä taulukkona',
  tableTime: 'Aika',
  tableValue: 'Arvo',
  tableMin: 'Pienin',
  tableMax: 'Suurin',
  tableCount: 'Mittauksia',
  tableSegmentBreak: 'Aukko: viiva katkeaa',

  // Gaps and restarts.
  gapsTitle: 'Datan menetykset valitulla välillä',
  gapsTableLabel: 'Menetysten taulukko',
  gapsNone: 'Vastaanottopalvelu ei kirjannut menetyksiä valitulla välillä.',
  gapsClockNote:
    'Menetysten ajat ovat vastaanottopalvelun kellon mukaan, kaavion pisteet auton kellon mukaan, joten aukon paikka on likimääräinen. Kun auto nukkuu, viiva jatkuu: uni ei ole menetys.',
  gapsTruncated: 'Välillä on enemmän menetyksiä kuin luettelo näyttää.',
  gapStart: 'Alkoi',
  gapDuration: 'Kesto',
  gapReason: 'Syy',
  gapCount: 'Määrä',
  gapKinds: {
    queue_full: 'Jono täyttyi, tietueita hylättiin',
    database_refused: 'Tietokanta hylkäsi tietueita',
    zmq_shortfall: 'Viestejä katosi vastaanottopalvelun sisällä',
  },
  gapCountUnits: {
    records: (count: string) => `${count} tietuetta`,
    messages: (count: string) => `${count} viestiä`,
  },
  restartsTitle: 'Vastaanottopalvelun käynnistykset',
  restartsNone: 'Ei käynnistyksiä valitulla välillä.',
  restartsNote: 'Käynnistys ei tarkoita, että dataa olisi kadonnut.',
  restartsTruncated: 'Välillä on enemmän käynnistyksiä kuin luettelo näyttää.',

  // States of the whole view.
  noDataYetTitle: 'Ei vielä dataa',
  noDataYetBody: 'Autosta ei ole vielä tallentunut akun tietoja. Data alkaa näkyä, kun auto herää ja lähettää tietoja.',
  failures: {
    database_unavailable: 'Tietokanta ei ole juuri nyt käytettävissä. Yritä hetken päästä uudelleen.',
    service_busy: 'Palvelu on juuri nyt varattu. Yritä hetken päästä uudelleen.',
    timeout: 'Haku kesti liian kauan. Yritä uudelleen.',
    server_error: 'Palvelussa tapahtui virhe.',
    data_contract_violation: 'Tallennettu data ei vastaa odotettua muotoa (esimerkiksi useampi auto).',
    unauthenticated: 'Kirjautuminen on vanhentunut. Kirjaudu uudelleen.',
    invalid_response:
      'Palvelimen vastaus ei ollut odotetun muotoinen. Lataa sivu uudelleen: sovellus voi olla vanhentunut.',
    network: 'Yhteys palveluun ei toimi. Tarkista verkkoyhteys.',
    rejected_request: 'Palvelin hylkäsi pyynnön. Lataa sivu uudelleen.',
  },
  staleUsableTitle: 'Päivitys epäonnistui',
} as const;
