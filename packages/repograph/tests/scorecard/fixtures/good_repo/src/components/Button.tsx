import React from 'react';

import { t } from '../i18n';

interface ButtonProps {
  onClick: () => void;
  label: string;
}

export const Button: React.FC<ButtonProps> = ({ onClick, label }) => {
  return (
    <button
      data-ui="action-button"
      onClick={onClick}
      className="btn btn-primary"
    >
      {t(label)}
    </button>
  );
};
