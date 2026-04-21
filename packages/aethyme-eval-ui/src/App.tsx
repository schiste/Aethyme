import { Routes, Route, Navigate } from "react-router-dom";
import Layout from "./components/Layout";
import Results from "./pages/Results";
import Matrix from "./pages/Matrix";
import Charts from "./pages/Charts";
import Repositories from "./pages/Repositories";
import RunEvals from "./pages/RunEvals";
import Batches from "./pages/Batches";

export default function App() {
  return (
    <Routes>
      <Route element={<Layout />}>
        <Route path="/" element={<Navigate to="/results" replace />} />
        <Route path="/results" element={<Results />} />
        <Route path="/matrix" element={<Matrix />} />
        <Route path="/charts" element={<Charts />} />
        <Route path="/repositories" element={<Repositories />} />
        <Route path="/run" element={<RunEvals />} />
      </Route>
    </Routes>
  );
}
