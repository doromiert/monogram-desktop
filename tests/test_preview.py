import unittest
from monogram_desktop.fixtures import PreviewState

class PreviewTests(unittest.TestCase):
    def test_conversation_edits_do_not_escape_the_preview_session(self):
        one=PreviewState();two=PreviewState()
        one.send("Local only")
        self.assertNotEqual(one.active.messages[-1].text,two.active.messages[-1].text)
        self.assertIsNone(one.send("  "))
    def test_search_and_folder_filters_intersect(self):
        s=PreviewState()
        self.assertTrue(all(c.group for c in s.filtered("room","Groups")))
        self.assertEqual(s.filtered("no-such-person"),[])
        self.assertTrue(all(c.unread for c in s.filtered(folder="Unread")))
        self.assertTrue(s.filtered("camera"))


class FolderTests(unittest.TestCase):
    def test_custom_folder_membership_and_search(self):
        state=PreviewState();state.custom_folders['Friends']={'mira','alex'}
        self.assertEqual({c.id for c in state.filtered(folder='Friends')},{'mira','alex'})
        self.assertEqual([c.id for c in state.filtered('camera','Friends')],['mira'])
