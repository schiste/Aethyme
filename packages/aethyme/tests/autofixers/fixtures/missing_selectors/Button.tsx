import React from 'react';

export function Button({ onClick, children }) {
  return (
    <button onClick={onClick}>
      {children}
    </button>
  );
}

export function Link({ href, children }) {
  return (
    <a href={href}>
      {children}
    </a>
  );
}
