import { Routes, Route, Navigate } from "react-router-dom";
import Layout from "./components/Layout";
import Results from "./pages/Results";
import Repositories from "./pages/Repositories";
import RunEvals from "./pages/RunEvals";

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route path="/" element={<Navigate to="/results" replace />} />
        <Route path="/results" element={<Results />} />
        <Route path="/repositories" element={<Repositories />} />
        <Route path="/run" element={<RunEvals />} />
      </Route>
    </Routes>
  );
}
