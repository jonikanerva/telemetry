import { Link } from '@tanstack/react-router';

import { fi } from '../i18n/fi.js';

export function NotFound() {
  return (
    <section aria-labelledby="not-found-title">
      <h1 id="not-found-title">{fi.notFoundTitle}</h1>
      <p>{fi.notFoundBody}</p>
      <Link to="/">{fi.notFoundLink}</Link>
    </section>
  );
}
