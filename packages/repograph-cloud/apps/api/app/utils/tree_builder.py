"""Utility for building hierarchical file trees from flat file lists."""

from typing import List, Dict, Any
from app.schemas.repository import FileTreeNode


class TreeBuilder:
    """Builds hierarchical file tree structure from flat file list."""

    def build_tree(self, files: List[Dict[str, Any]]) -> List[FileTreeNode]:
        """
        Build hierarchical tree from flat file list.

        Args:
            files: List of file dicts with keys: file_path, language, size_bytes, line_count

        Returns:
            List of FileTreeNode objects representing root-level folders and files

        Example:
            Input: [
                {"file_path": "src/main.py", "language": "Python", "line_count": 100},
                {"file_path": "src/utils/helper.py", "language": "Python", "line_count": 50},
                {"file_path": "README.md", "language": "Markdown", "line_count": 20}
            ]

            Output: [
                FileTreeNode(
                    name="src",
                    path="src",
                    type="folder",
                    children=[
                        FileTreeNode(name="main.py", path="src/main.py", type="file", ...),
                        FileTreeNode(
                            name="utils",
                            path="src/utils",
                            type="folder",
                            children=[
                                FileTreeNode(name="helper.py", path="src/utils/helper.py", type="file", ...)
                            ]
                        )
                    ]
                ),
                FileTreeNode(name="README.md", path="README.md", type="file", ...)
            ]
        """
        if not files:
            return []

        # Build tree structure using a dictionary
        root: Dict[str, Any] = {"children": {}}

        for file_data in files:
            file_path = file_data.get("file_path", "")
            if not file_path:
                continue

            # Split path into parts
            parts = file_path.split("/")
            current = root

            # Navigate/create folder structure
            for i, part in enumerate(parts):
                if i == len(parts) - 1:
                    # This is the file (leaf node)
                    current["children"][part] = {
                        "name": part,
                        "path": file_path,
                        "type": "file",
                        "language": file_data.get("language"),
                        "size_bytes": file_data.get("size_bytes"),
                        "line_count": file_data.get("line_count"),
                        "children": None
                    }
                else:
                    # This is a folder
                    if part not in current["children"]:
                        current["children"][part] = {
                            "name": part,
                            "path": "/".join(parts[:i + 1]),
                            "type": "folder",
                            "language": None,
                            "size_bytes": None,
                            "line_count": None,
                            "children": {}
                        }
                    current = current["children"][part]

        # Convert dict tree to FileTreeNode list
        return self._dict_to_nodes(root["children"])

    def _dict_to_nodes(self, children_dict: Dict[str, Any]) -> List[FileTreeNode]:
        """
        Convert dictionary tree structure to list of FileTreeNode objects.

        Args:
            children_dict: Dictionary of child nodes

        Returns:
            List of FileTreeNode objects, sorted with folders first
        """
        nodes: List[FileTreeNode] = []

        # Sort keys: folders first, then files, alphabetically within each group
        sorted_keys = sorted(
            children_dict.keys(),
            key=lambda k: (
                0 if children_dict[k]["type"] == "folder" else 1,  # Folders first
                k.lower()  # Then alphabetically
            )
        )

        for key in sorted_keys:
            node_data = children_dict[key]

            # Recursively convert children if folder
            children = None
            if node_data["type"] == "folder" and isinstance(node_data.get("children"), dict):
                children = self._dict_to_nodes(node_data["children"])

            node = FileTreeNode(
                name=node_data["name"],
                path=node_data["path"],
                type=node_data["type"],
                language=node_data.get("language"),
                size_bytes=node_data.get("size_bytes"),
                line_count=node_data.get("line_count"),
                children=children
            )
            nodes.append(node)

        return nodes

    def count_folders(self, tree: List[FileTreeNode]) -> int:
        """
        Count total number of folders in tree.

        Args:
            tree: List of root FileTreeNode objects

        Returns:
            Total folder count
        """
        count = 0
        for node in tree:
            if node.type == "folder":
                count += 1
                if node.children:
                    count += self.count_folders(node.children)
        return count

    def count_files(self, tree: List[FileTreeNode]) -> int:
        """
        Count total number of files in tree.

        Args:
            tree: List of root FileTreeNode objects

        Returns:
            Total file count
        """
        count = 0
        for node in tree:
            if node.type == "file":
                count += 1
            elif node.children:
                count += self.count_files(node.children)
        return count
