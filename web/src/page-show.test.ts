import { describe, expect, it, vi } from 'vitest';

import { createPageShowHandler } from './page-show.js';

describe('createPageShowHandler', () => {
  it('reloads a page that the back-forward cache restored', () => {
    const reload = vi.fn();
    createPageShowHandler(reload)({ persisted: true });
    expect(reload).toHaveBeenCalledOnce();
  });

  it('does not reload a page that was loaded normally', () => {
    const reload = vi.fn();
    createPageShowHandler(reload)({ persisted: false });
    expect(reload).not.toHaveBeenCalled();
  });
});
