import React from 'react';

export function UserGreeting({ name }) {
  return (
    <div>
      <h1>Welcome Back</h1>
      <p>Hello, {name}! How are you today?</p>
      <button>Click Me</button>
    </div>
  );
}
