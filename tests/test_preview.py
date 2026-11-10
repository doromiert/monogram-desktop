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
        from monogram_desktop.fixtures import FolderRules
        state=PreviewState();state.custom_folders['Friends']=FolderRules(include={'mira','alex'})
        self.assertEqual({c.id for c in state.filtered(folder='Friends')},{'mira','alex'})
        self.assertEqual([c.id for c in state.filtered('camera','Friends')],['mira'])
        state.custom_folders['All']=FolderRules(include={'mira'})
        self.assertEqual([c.id for c in state.filtered(folder='All')],['mira'])

    def test_rules_follow_new_chats_and_mobile_override_order(self):
        from monogram_desktop.fixtures import FolderRules,Chat
        state=PreviewState();rule=FolderRules({'groups'},exclude_read=True,exclude_muted=True,exclude_archived=True)
        state.custom_folders['Active groups']=rule
        new=Chat('new','New room','','N','blue',unread=1,group=True);state.chats.append(new)
        self.assertIn(new,state.filtered(folder='Active groups'))
        new.unread=0;self.assertNotIn(new,state.filtered(folder='Active groups'))
        new.unread_mentions=1;new.notifications='forever';self.assertIn(new,state.filtered(folder='Active groups'))
        new.archived=True;self.assertNotIn(new,state.filtered(folder='Active groups'))
        rule.include.add('new');self.assertIn(new,state.filtered(folder='Active groups'))
        rule.exclude.add('new');self.assertNotIn(new,state.filtered(folder='Active groups'))

    def test_folder_reorder_preserves_membership(self):
        state=PreviewState();original=state.filtered(folder='Groups')
        state.reorder_folder('Groups','All');self.assertEqual(state.folder_order,['Groups','All','Unread'])
        state.reorder_folder('Groups','Unread',True);self.assertEqual(state.folder_order,['All','Unread','Groups'])
        self.assertEqual(state.filtered(folder='Groups'),original)


class AmountTests(unittest.TestCase):
    def test_decimal_edits_and_review_eligibility(self):
        from monogram_desktop.amounts import valid_edit,positive
        for value in ("", ".", "0,", "25", "0.000000000001"):self.assertTrue(valid_edit(value))
        for value in ("hello", "1e5", "-2", "1.2.3", "0.0000000000001", "١٢"):self.assertFalse(valid_edit(value))
        for value in ("", ".", "0", "0,0"):self.assertFalse(positive(value))
        for value in (".1", "0,25", "0.000000000001"):self.assertTrue(positive(value))


class WalletTests(unittest.TestCase):
    def test_source_mesh_bounds_projection_and_seed(self):
        from monogram_desktop.wallet_material import mesh,matrices,JavaRandom,stars
        import numpy as np
        face=mesh("card_back.binobj")
        self.assertEqual(face.shape[1],6)
        self.assertAlmostEqual(float(abs(face[:,2]).max()),1,places=4)
        _,mvp=matrices(500,305,0,0)
        corner=mvp@np.array([-.0125,.618561,1.,1.])
        self.assertLess(abs(corner[0]/corner[3]),1.05)
        self.assertLess(abs(corner[1]/corner[3]),1.05)
        import math,itertools
        for x,y in itertools.product((-.2618,0,.2618),repeat=2):
            _,tilted=matrices(500,305,x,y)
            for fy,fz in itertools.product((-.593561,.618561),(-1.,1.)):
                point=tilted@np.array([-.0125,fy,fz,1.])
                self.assertLess(abs(point[0]/point[3]),1)
                self.assertLess(abs(point[1]/point[3]),1)
        _,wide=matrices(360,154,0,0)
        corner=wide@np.array([-.0125,.618561,1.,1.])
        self.assertLess(abs(corner[0]/corner[3]),1.05)
        self.assertLess(abs(corner[1]/corner[3]),1.05)
        self.assertAlmostEqual(JavaRandom(0).random(),.7309677600860596)
        self.assertEqual(stars(),stars())
