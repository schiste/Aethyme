import React from 'react';

interface ButtonProps {
  onClick: () => void;
}

export const BadButton: React.FC<ButtonProps> = ({ onClick }) => {
  return (
    <button onClick={onClick} className="btn">
      Click Me Now
    </button>
  );
};

export const AnotherButton = () => {
  return (
    <form>
      <input type="text" placeholder="Enter your name here" />
      <button type="submit">Submit the Form</button>
    </form>
  );
};
